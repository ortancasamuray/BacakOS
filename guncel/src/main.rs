//! bacak-guncel — BacakOS otomatik güncelleyici.
//!
//!   bacak-guncel calis              tam döngü (bacak-guncel.timer çağırır)
//!   bacak-guncel onar [--cevrimdisi] yalnız yarım kalan kurulumu onar
//!   bacak-guncel denetle            güncelleme var mı (indirmez, kurmaz)
//!   bacak-guncel durum              son durumu yaz
//!
//! Döngü: kilit → onarım → her eklenti için denetle → indir → doğrula →
//! (hepsi bitince) pil/boş alan denetimi → yalnız tam indirilmiş eklentileri
//! kur. Kurulum başlamadan "yarım" işareti diske yazılır; temiz bitince
//! silinir. İşaret kalırsa sonraki açılışta giriş ekranından önce
//! bacak-guncel-onar.service onarır.

mod ayar;
mod durum;
mod eklenti;
#[macro_use]
mod gunluk;
mod komut;
mod koruma;

use ayar::Ayar;
use durum::{Durum, Evre};
use eklenti::{Eklenti, Plan};
use komut::Hata;
use std::fs::File;
use std::os::fd::AsRawFd;
use std::process::ExitCode;

const KILIT: &str = "/run/lock/bacak-guncel.lock";

const YARDIM: &str = "\
bacak-guncel — BacakOS otomatik güncelleyici

Kullanım:
  bacak-guncel calis               denetle, tamamen indir, doğrula, kur
  bacak-guncel onar [--cevrimdisi] yarım kalmış kurulumu onar
  bacak-guncel denetle             bekleyen güncellemeleri listele
  bacak-guncel durum               son çalışmanın durumu

Ayarlar: /etc/bacak-guncel/bacak-guncel.conf
Günlük:  journalctl -u bacak-guncel -u bacak-guncel-onar
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let komut = args.first().map(String::as_str).unwrap_or("");
    // `bacak-guncel durum | head` gibi kesilen çıktıda panik değil sessiz çıkış.
    // SAFETY: tek iş parçacıklıyken sinyal kipini değiştirmek.
    unsafe { libc::signal(libc::SIGPIPE, libc::SIG_DFL) };
    match komut {
        "durum" => return durum_yaz(),
        "calis" | "onar" | "denetle" => {}
        "-h" | "--help" | "--yardim" | "yardim" => {
            print!("{YARDIM}");
            return ExitCode::SUCCESS;
        }
        _ => {
            eprint!("{YARDIM}");
            return ExitCode::from(2);
        }
    }
    // SAFETY: salt okunur sistem çağrısı.
    if unsafe { libc::geteuid() } != 0 {
        hata!("root yetkisi gerekli (sudo bacak-guncel {komut})");
        return ExitCode::from(1);
    }
    let Some(_kilit) = kilit_al() else {
        bilgi!("başka bir bacak-guncel çalışıyor, çıkılıyor");
        return ExitCode::SUCCESS;
    };
    komut::sinyalleri_kur();
    let ayar = Ayar::yukle();
    let mut eklentiler = eklenti::yukle(&ayar);
    let mut durum = Durum::yukle();

    let sonuc = match komut {
        "onar" => onar(&mut eklentiler, &mut durum, args.iter().any(|a| a == "--cevrimdisi")),
        "denetle" => denetle(&mut eklentiler),
        _ if !ayar.etkin => {
            bilgi!("otomatik güncelleme kapalı (etkin = hayir); yalnız onarım yapılıyor");
            onar(&mut eklentiler, &mut durum, false)
        }
        _ => calis(&ayar, &mut eklentiler, &mut durum),
    };
    match sonuc {
        Ok(()) => ExitCode::SUCCESS,
        Err(Hata::Kesildi) => {
            bilgi!("durdurma isteği: kalan adımlar sonraki açılışa bırakıldı");
            ExitCode::SUCCESS
        }
        Err(e) => {
            hata!("{e}");
            ExitCode::from(1)
        }
    }
}

fn kilit_al() -> Option<File> {
    let _ = std::fs::create_dir_all("/run/lock");
    let f = File::create(KILIT).ok()?;
    // SAFETY: açık dosya tanıtıcısı üzerinde flock.
    let r = unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    (r == 0).then_some(f)
}

fn onar(eklentiler: &mut [Box<dyn Eklenti>], durum: &mut Durum, cevrimdisi: bool) -> komut::Sonuc<()> {
    let yarim = durum::yarim_var();
    if yarim {
        uyari!("önceki güncelleme yarıda kalmış, onarılıyor");
    }
    let onceki = durum.evre;
    durum.evre(Evre::Onariliyor, None);
    let mut basarisiz = Vec::new();
    for e in eklentiler.iter_mut() {
        if let Err(h) = e.onar(cevrimdisi) {
            hata!("{}: onarım başarısız: {h}", e.ad());
            basarisiz.push(e.ad());
        }
    }
    if !basarisiz.is_empty() {
        durum.son_hata = Some(format!("onarım başarısız: {}", basarisiz.join(", ")));
        durum.evre(Evre::Bosta, None);
        // İşaret kalır: bir sonraki açılış / çalışma yeniden dener.
        return komut::hata(format!("onarım başarısız: {}", basarisiz.join(", ")));
    }
    if yarim {
        durum::yarim_kaldir()?;
        bilgi!("onarım tamam, sistem tutarlı");
    }
    durum.evre(if onceki == Evre::Indirildi { Evre::Indirildi } else { Evre::Bosta }, None);
    Ok(())
}

fn denetle(eklentiler: &mut [Box<dyn Eklenti>]) -> komut::Sonuc<()> {
    let mut var = false;
    for e in eklentiler.iter_mut() {
        match e.denetle()? {
            Some(p) => {
                var = true;
                println!("[{}] {} güncelleme:", e.ad(), p.ogeler.len());
                for o in &p.ogeler {
                    println!("  {o}");
                }
            }
            None => println!("[{}] güncel", e.ad()),
        }
    }
    if !var {
        println!("Sistem güncel.");
    }
    Ok(())
}

fn calis(ayar: &Ayar, eklentiler: &mut [Box<dyn Eklenti>], durum: &mut Durum) -> komut::Sonuc<()> {
    durum.son_deneme = durum::simdi();
    // 1. Önce tutarlılık: yarım kurulum varsa hiçbir yeni şeye başlanmaz.
    if let Err(e) = onar(eklentiler, durum, false) {
        hata_kaydet(durum, &e);
        return Err(e);
    }

    // 2. Denetle + tamamen indir + doğrula. Kurulum burada başlamaz.
    let mut hazir: Vec<(usize, Plan)> = Vec::new();
    let mut hatalar: Vec<String> = Vec::new();
    for (i, e) in eklentiler.iter_mut().enumerate() {
        let ad = e.ad();
        match hazirla(e.as_mut(), durum) {
            Ok(Some(plan)) => hazir.push((i, plan)),
            Ok(None) => bilgi!("{ad}: güncel"),
            Err(Hata::Kesildi) => {
                durum.evre(Evre::Bosta, None);
                return Err(Hata::Kesildi);
            }
            Err(h) => {
                hata!("{ad}: {h}");
                hatalar.push(format!("{ad}: {h}"));
            }
        }
    }

    if hazir.is_empty() {
        return bitir(durum, hatalar, Vec::new());
    }
    durum.bekleyen = hazir.iter().flat_map(|(_, p)| p.ogeler.clone()).collect();
    durum.evre(Evre::Indirildi, None);

    // 3. Kurulum ön koşulları: hepsi indirildi; şimdi güç ve alan.
    if !koruma::guc_yeterli(ayar.en_az_pil) {
        bilgi!("pil %{}'in altında ve şarjda değil: kurulum prize takılınca yapılacak", ayar.en_az_pil);
        return Ok(());
    }
    let gereken: u64 = hazir.iter().map(|(i, p)| eklentiler[*i].gereken_alan(p)).sum::<u64>()
        + (ayar.en_az_bos_mb << 20);
    match koruma::bos_alan("/") {
        Ok(bos) if bos < gereken => {
            let m = format!("kök dosya sisteminde yer yok ({} MB boş, {} MB gerekli)", bos >> 20, gereken >> 20);
            uyari!("{m}: kurulum ertelendi");
            durum.son_hata = Some(m);
            durum.kaydet();
            return Ok(());
        }
        Err(e) => uyari!("boş alan ölçülemedi: {e}"),
        _ => {}
    }
    if komut::durduruldu() {
        return Err(Hata::Kesildi);
    }

    // 4. Kurulum. Yarım işareti diske yazılmadan dpkg'ye dokunulmaz.
    let mut guncellenen = Vec::new();
    let mut yeniden_baslat = false;
    for (i, plan) in &hazir {
        let e = &mut eklentiler[*i];
        let ad = e.ad();
        durum::yarim_isaretle(&format!("{ad}\n{}\n", plan.ogeler.join("\n")))?;
        durum.evre(Evre::Uygulaniyor, Some(&ad));
        match e.uygula(plan) {
            Ok(()) => {
                durum::yarim_kaldir()?;
                bilgi!("{ad}: {} güncelleme kuruldu", plan.ogeler.len());
                guncellenen.extend(plan.ogeler.iter().cloned());
                yeniden_baslat |= e.yeniden_baslatma_ister();
            }
            Err(h) => {
                hata!("{ad}: kurulum başarısız: {h}");
                hatalar.push(format!("{ad}: {h}"));
                // Hemen onarmayı dene; olmazsa işaret açılışta onarımı tetikler.
                match e.onar(false) {
                    Ok(()) => durum::yarim_kaldir()?,
                    Err(o) => hata!("{ad}: onarım da başarısız, açılışta yeniden denenecek: {o}"),
                }
            }
        }
        if komut::durduruldu() {
            break;
        }
    }
    if yeniden_baslat && !guncellenen.is_empty() {
        koruma::yeniden_baslatma_gerekli(&guncellenen);
    }
    durum.bekleyen.clear();
    bitir(durum, hatalar, guncellenen)
}

/// Bir eklentiyi kuruluma hazırlar: güncelleme yoksa None; varsa tamamen
/// indirilip doğrulanmış planı döndürür.
fn hazirla(e: &mut dyn Eklenti, durum: &mut Durum) -> komut::Sonuc<Option<Plan>> {
    let ad = e.ad();
    durum.evre(Evre::Denetleniyor, Some(&ad));
    let Some(plan) = e.denetle()? else {
        return Ok(None);
    };
    bilgi!("{ad}: {} güncelleme bulundu", plan.ogeler.len());
    for o in &plan.ogeler {
        bilgi!("  {o}");
    }
    durum.evre(Evre::Indiriliyor, Some(&ad));
    // Kesilen/bozuk indirme bir kez daha denenir; apt kaldığı yerden sürer.
    for deneme in 1..=2 {
        match e.indir(&plan) {
            Err(Hata::Kesildi) => return Err(Hata::Kesildi),
            Err(h) => uyari!("{ad}: indirme {deneme}. denemede başarısız: {h}"),
            Ok(()) => {}
        }
        if e.dogrula(&plan)? {
            return Ok(Some(plan));
        }
    }
    komut::hata("indirme tamamlanamadı; kurulum yapılmadı, sonraki çalışmada sürecek")
}

fn bitir(durum: &mut Durum, hatalar: Vec<String>, guncellenen: Vec<String>) -> komut::Sonuc<()> {
    if !guncellenen.is_empty() {
        durum.son_guncellenen = guncellenen;
    }
    if hatalar.is_empty() {
        durum.son_basari = Some(durum::simdi());
        durum.son_hata = None;
        durum.ardisik_hata = 0;
        durum.evre(Evre::Bosta, None);
        Ok(())
    } else {
        // Ayrıntılar yukarıda günlüğe yazıldı; durum dosyası hepsini tutar.
        hata_kaydet(durum, &Hata::Mesaj(hatalar.join("; ")));
        Err(Hata::Mesaj(format!("{} eklentide hata, sonraki çalışmada yeniden denenecek", hatalar.len())))
    }
}

fn hata_kaydet(durum: &mut Durum, e: &Hata) {
    durum.son_hata = Some(e.to_string());
    durum.ardisik_hata += 1;
    durum.evre(Evre::Bosta, None);
}

fn tarih(t: u64) -> String {
    std::process::Command::new("date")
        .args(["-d", &format!("@{t}"), "+%F %T"])
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| t.to_string())
}

fn durum_yaz() -> ExitCode {
    let d = Durum::yukle();
    let evre = match d.evre {
        Evre::Bosta => "boşta",
        Evre::Onariliyor => "onarılıyor",
        Evre::Denetleniyor => "denetleniyor",
        Evre::Indiriliyor => "indiriliyor",
        Evre::Indirildi => "indirildi, kurulum bekliyor",
        Evre::Uygulaniyor => "kuruluyor",
    };
    println!("Durum        : {evre}{}", d.eklenti.map(|e| format!(" ({e})")).unwrap_or_default());
    if durum::yarim_var() {
        println!("UYARI        : yarım kalmış kurulum var, onarım bekliyor");
    }
    if d.son_deneme > 0 {
        println!("Son deneme   : {}", tarih(d.son_deneme));
    }
    match d.son_basari {
        Some(t) => println!("Son başarı   : {}", tarih(t)),
        None => println!("Son başarı   : henüz yok"),
    }
    if let Some(h) = &d.son_hata {
        println!("Son hata     : {h} ({} kez üst üste)", d.ardisik_hata);
    }
    if !d.bekleyen.is_empty() {
        println!("Bekleyen     :");
        d.bekleyen.iter().for_each(|b| println!("  {b}"));
    }
    if !d.son_guncellenen.is_empty() {
        println!("Son kurulan  :");
        d.son_guncellenen.iter().for_each(|b| println!("  {b}"));
    }
    if std::path::Path::new("/run/reboot-required").exists() {
        println!("Yeniden başlatma gerekli.");
    }
    ExitCode::SUCCESS
}
