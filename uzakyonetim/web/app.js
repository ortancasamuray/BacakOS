// SPDX-License-Identifier: GPL-3.0-or-later
// Uzak Yönetim web panel. No framework, no inline script (strict CSP), and
// every server/agent-provided string goes into the DOM via textContent.
"use strict";

let csrf = "";
let refreshTimer = null;
const $ = (id) => document.getElementById(id);

function el(tag, attrs = {}, ...children) {
  const n = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs)) {
    if (k === "class") n.className = v;
    else if (k.startsWith("on")) n.addEventListener(k.slice(2), v);
    else if (v !== undefined && v !== null && v !== false) n.setAttribute(k, v === true ? "" : v);
  }
  for (const c of children.flat()) {
    if (c === null || c === undefined || c === false) continue;
    n.append(c instanceof Node ? c : document.createTextNode(String(c)));
  }
  return n;
}

async function api(method, path, body) {
  const opts = { method, headers: {}, credentials: "same-origin" };
  if (method !== "GET") opts.headers["x-uzy-csrf"] = csrf;
  if (body !== undefined) {
    opts.headers["content-type"] = "application/json";
    opts.body = JSON.stringify(body);
  }
  const r = await fetch(path, opts);
  if (r.status === 401 && path !== "/api/giris") {
    showLogin();
    throw new Error("Oturum sona erdi");
  }
  const data = await r.json().catch(() => ({}));
  if (!r.ok) throw new Error(data.hata || `Hata ${r.status}`);
  return data;
}

const fmtTime = (s) => (s ? new Date(s * 1000).toLocaleString("tr-TR") : "—");
function ago(s) {
  if (!s) return "hiç";
  const d = Math.max(0, Math.floor(Date.now() / 1000 - s));
  if (d < 60) return `${d} sn önce`;
  if (d < 3600) return `${Math.floor(d / 60)} dk önce`;
  if (d < 86400) return `${Math.floor(d / 3600)} sa önce`;
  return fmtTime(s);
}

// ---- login -----------------------------------------------------------------

function showLogin() {
  clearInterval(refreshTimer);
  $("uygulama").hidden = true;
  $("kurulum").hidden = true;
  $("giris").hidden = false;
}

function showApp(name) {
  $("giris").hidden = true;
  $("kurulum").hidden = true;
  $("uygulama").hidden = false;
  $("yonetici").textContent = name;
  loadMachines();
  clearInterval(refreshTimer);
  refreshTimer = setInterval(loadMachines, 10000);
}

$("giris-formu").addEventListener("submit", async (e) => {
  e.preventDefault();
  const f = new FormData(e.target);
  $("giris-hata").hidden = true;
  try {
    const r = await api("POST", "/api/giris", {
      kullanici: f.get("kullanici").trim(), parola: f.get("parola"), kod: f.get("kod").trim(),
    });
    csrf = r.csrf;
    e.target.reset();
    showApp(r.yonetici);
  } catch (err) {
    $("giris-hata").textContent = err.message;
    $("giris-hata").hidden = false;
  }
});

$("cikis").addEventListener("click", async () => {
  try { await api("POST", "/api/cikis"); } catch (_) { /* already gone */ }
  csrf = "";
  showLogin();
});

// ---- tabs ------------------------------------------------------------------

document.querySelectorAll(".sekme").forEach((b) => b.addEventListener("click", () => {
  document.querySelectorAll(".sekme").forEach((x) => x.classList.toggle("etkin", x === b));
  for (const s of ["makineler", "denetim", "ayarlar"]) $(`sekme-${s}`).hidden = s !== b.dataset.sekme;
  if (b.dataset.sekme === "denetim") loadAudit();
  if (b.dataset.sekme === "ayarlar") { loadSettings(); loadAdmins(); }
}));

// ---- machines grid ---------------------------------------------------------

async function loadMachines() {
  let list;
  try { list = await api("GET", "/api/makineler"); } catch (_) { return; }
  $("bos-filo").hidden = list.length > 0;
  $("izgara").replaceChildren(...list.map(machineCard));
}

function screenImg(m, cls = "ekran") {
  if (!m.son_ekran) return el("div", { class: cls }, "Görüntü yok");
  return el("img", { class: cls, alt: `${m.ad} ekranı`, src: `/api/makineler/${m.id}/ekran?t=${m.son_ekran}` });
}

function machineCard(m) {
  const users = el("button", { disabled: !m.cevrimici || m.iptal, onclick: (e) => { e.stopPropagation(); openUsers(m); } }, "Kullanıcılar");
  return el("div", { class: `makine${m.iptal ? " iptal" : ""}`, onclick: () => openScreen(m) },
    screenImg(m),
    el("div", { class: "makine-bilgi" },
      el("div", { class: "ad" }, el("span", { class: `nokta${m.cevrimici ? " acik" : ""}` }), m.ad, m.iptal ? el("span", { class: "rozet" }, "iptal") : null),
      el("div", { class: "soluk kucuk" },
        m.cevrimici ? "Çevrimiçi" : `Son görülme: ${ago(m.son_gorulme)}`,
        " · ", m.son_ekran ? (m.son_ekran_oturum ? `Oturum: ${m.son_ekran_oturum}` : "Giriş ekranı") : "—"),
      el("div", { class: "soluk kucuk" }, m.isletim_sistemi || ""),
      el("div", { class: "satir" }, users)));
}

// ---- dialog ----------------------------------------------------------------

function openDialog(...content) {
  $("pencere-icerik").replaceChildren(...content.filter((c) => c !== null && c !== undefined));
  if (!$("pencere").open) $("pencere").showModal();
}
const closeDialog = () => $("pencere").close();
const header = (title, ...extra) =>
  el("div", { class: "pencere-ust" }, el("h2", {}, title), ...extra, el("button", { onclick: closeDialog }, "Kapat"));

/** Screen window: live/history screenshots and the machine itself. */
async function openScreen(m) {
  let big = screenImg(m);
  const history = el("div", { class: "gecmis" });
  const status = el("p", { class: "soluk kucuk" });
  const shoot = el("button", { disabled: !m.cevrimici, onclick: async () => {
    shoot.disabled = true;
    try {
      const r = await api("POST", `/api/makineler/${m.id}/ekran-al`);
      m.son_ekran = r.zaman;
      openScreen(m);
    } catch (e) { status.textContent = e.message; shoot.disabled = false; }
  } }, "Şimdi ekran al");
  const users = el("button", { disabled: !m.cevrimici || m.iptal, onclick: () => openUsers(m) }, "Kullanıcılar");
  const revoke = el("button", { class: "tehlike", disabled: m.iptal, onclick: async () => {
    if (!confirm(`${m.ad} filodan çıkarılsın mı? Ajanın sertifikası iptal edilir; yeniden eklemek için yeni katılım kodu gerekir.`)) return;
    try { await api("POST", `/api/makineler/${m.id}/iptal`); closeDialog(); loadMachines(); } catch (e) { status.textContent = e.message; }
  } }, "Filodan çıkar");

  openDialog(
    header(`${m.ad} — Ekran`, shoot, users, revoke),
    big, history, status,
    el("p", { class: "soluk kucuk" }, `${m.isletim_sistemi || ""} · ajan ${m.ajan_surumu || "?"} · kayıt ${fmtTime(m.kayit_zamani)}`),
    policyLine(m.politika));

  api("GET", `/api/makineler/${m.id}/gecmis`).then((ts) => {
    history.replaceChildren(...ts.slice(0, 30).map((t) => el("button", { onclick: () => {
      const img = el("img", { class: "ekran", alt: "geçmiş", src: `/api/makineler/${m.id}/gecmis/${t}` });
      big.replaceWith(img);
      big = img;
    } }, new Date(t * 1000).toLocaleTimeString("tr-TR"))));
  }).catch(() => {});
}

/** Users window: accounts on the machine, Bacak Onay, new account. */
function openUsers(m) {
  const box = el("div", {}, el("p", { class: "soluk" }, "Kullanıcılar yükleniyor…"));
  openDialog(
    header(`${m.ad} — Kullanıcılar`, el("button", { onclick: () => openScreen(m) }, "Ekran")),
    policyLine(m.politika),
    el("div", { class: "iki-sutun" }, box, accountForm(m)));
  loadUsers(m, box);
}

function policyLine(p) {
  if (!p || p.hesap_acma === undefined) return null;
  const off = [["hesap_acma", "hesap açma"], ["hesap_silme", "hesap silme"], ["bacakonay", "Bacak Onay"], ["ekran_izleme", "ekran izleme"]]
    .filter(([k]) => p[k] === false).map(([, v]) => v);
  return off.length ? el("p", { class: "uyari kucuk" }, `Bu makinenin yerel politikası kapalı tutuyor: ${off.join(", ")}`) : null;
}

async function loadUsers(m, box) {
  try {
    const list = await api("GET", `/api/makineler/${m.id}/kullanicilar`);
    const rows = list.map((u) => el("tr", {},
      el("td", {}, u.kullanici, u.tam_ad ? el("div", { class: "soluk kucuk" }, u.tam_ad) : null),
      el("td", {}, u.bacakonay ? el("span", { class: "rozet onay" }, "Bacak Onay") : el("span", { class: "rozet" }, "yalnız parola"),
        u.yonetici ? el("span", { class: "rozet yonetici" }, " yönetici") : null),
      el("td", { class: "satir" }, u.bacakonay
        ? el("button", { onclick: () => removeTotp(m, u.kullanici, box) }, "Bacak Onay kaldır")
        : el("button", { onclick: () => beginTotp(m, u.kullanici) }, "Bacak Onay kur"),
        u.yonetici ? null : el("button", { class: "tehlike", onclick: () => deleteAccount(m, u.kullanici, box) }, "Sil"))));
    box.replaceChildren(rows.length
      ? el("table", { class: "tablo" }, el("tbody", {}, rows))
      : el("p", { class: "soluk" }, "Normal kullanıcı yok."));
  } catch (e) {
    box.replaceChildren(el("p", { class: "hata" }, e.message));
  }
}

async function removeTotp(m, user, box) {
  if (!confirm(`${user} için Bacak Onay kaldırılsın mı? Giriş yalnız parolayla yapılır.`)) return;
  try { await api("POST", `/api/makineler/${m.id}/bacakonay/kaldir`, { kullanici: user }); loadUsers(m, box); }
  catch (e) { alert(e.message); }
}

async function deleteAccount(m, user, box) {
  if (!confirm(`${user} hesabı ${m.ad} makinesinden silinsin mi?\nEv dizini ve içindeki tüm dosyalar da silinir; geri alınamaz.`)) return;
  try { await api("POST", `/api/makineler/${m.id}/hesap/sil`, { kullanici: user }); loadUsers(m, box); }
  catch (e) { alert(e.message); }
}

function accountForm(m) {
  const err = el("p", { class: "hata" });
  const form = el("form", { class: "kart", autocomplete: "off" },
    el("h3", {}, "Yeni hesap"),
    el("label", {}, "Kullanıcı adı", el("input", { name: "kullanici", required: true, pattern: "[a-z_][a-z0-9_-]{0,31}", placeholder: "ayse" })),
    el("label", {}, "Ad Soyad", el("input", { name: "tam_ad", placeholder: "Ayşe Yılmaz" })),
    el("label", {}, "İlk parola (en az 8 karakter)", el("input", { name: "parola", type: "password", required: true, minlength: 8, autocomplete: "new-password" })),
    el("label", { class: "satir" }, el("input", { name: "bacakonay", type: "checkbox", checked: true, style: undefined }), " Bacak Onay iki adımlı girişi kur"),
    err, el("button", { class: "birincil", type: "submit" }, "Hesabı aç"));
  form.addEventListener("submit", async (e) => {
    e.preventDefault();
    const f = new FormData(form);
    const user = f.get("kullanici").trim();
    err.textContent = "";
    try {
      const r = await api("POST", `/api/makineler/${m.id}/hesap`, {
        kullanici: user, tam_ad: f.get("tam_ad").trim(), parola: f.get("parola"), bacakonay: f.get("bacakonay") === "on",
      });
      form.reset();
      if (r.qr) showQr(m, user, r); else openUsers(m);
    } catch (ex) { err.textContent = ex.message; }
  });
  return form;
}

async function beginTotp(m, user) {
  try { showQr(m, user, await api("POST", `/api/makineler/${m.id}/bacakonay`, { kullanici: user })); }
  catch (e) { alert(e.message); }
}

/** QR + confirmation: 2FA is only switched on once the phone proves it has the secret. */
function showQr(m, user, r) {
  const msg = el("p", { class: "hata" });
  const code = el("input", { inputmode: "numeric", maxlength: 8, placeholder: "Telefondaki 6 haneli kod", autocomplete: "one-time-code" });
  const ok = el("button", { class: "birincil", onclick: async () => {
    msg.textContent = "";
    try {
      await api("POST", `/api/makineler/${m.id}/bacakonay/onayla`, { kullanici: user, kod: code.value.trim() });
      openDialog(header(`${user} — Bacak Onay`), el("p", {}, `✓ ${user}@${m.ad} için iki adımlı giriş etkin. Turan giriş ekranı paroladan sonra kodu soracak.`),
        el("button", { onclick: () => openUsers(m) }, "Kullanıcılara dön"));
    } catch (e) { msg.textContent = e.message; }
  } }, "Onayla ve etkinleştir");
  openDialog(header(`${user}@${m.ad} — Bacak Onay kaydı`),
    el("p", {}, "Kullanıcı telefonunda Bacak Onay → QR tara ile bu kodu okutsun, ardından uygulamada görünen kodu aşağıya girin."),
    el("img", { class: "qr", alt: "Bacak Onay QR kodu", src: r.qr }),
    el("p", { class: "soluk kucuk" }, "Kamera yoksa elle giriş anahtarı:"), el("div", { class: "kod-kutu" }, r.anahtar),
    el("p", { class: "uyari kucuk" }, "Bu QR bir kez gösterilir ve sunucuda saklanmaz. 10 dakika içinde onaylanmazsa geçersiz olur."),
    el("label", {}, "Doğrulama kodu", code), msg, ok);
  code.focus();
}

// ---- add machine -----------------------------------------------------------

$("makine-ekle").addEventListener("click", async () => {
  try {
    const r = await api("POST", "/api/katilim-kodu", { gecerlilik_dk: 60 });
    const copy = el("button", { onclick: async () => {
      try { await navigator.clipboard.writeText(r.komut); copy.textContent = "Kopyalandı ✓"; } catch (_) { copy.textContent = "Elle kopyalayın"; }
    } }, "Komutu kopyala");
    openDialog(header("Makine ekle"),
      el("p", {}, "Eklenecek BacakOS makinesinde (uzakyonetim-ajan kurulu) şu komutu çalıştırın:"),
      el("div", { class: "kod-kutu" }, r.komut), el("div", { class: "satir" }, copy),
      el("p", { class: "soluk kucuk" }, `Kod tek kullanımlıktır ve ${r.gecerlilik_dk} dakika geçerlidir. Sunucunun CA parmak izini içerir; makine yalnızca bu sunucuya bağlanır.`),
      el("p", { class: "soluk kucuk" }, "Ardından makinede: sudo systemctl enable --now uzakyonetim-ajan"));
  } catch (e) { alert(e.message); }
});

// ---- settings / audit ------------------------------------------------------

async function loadSettings() {
  try { $("aralik").value = String((await api("GET", "/api/ayarlar")).ekran_araligi_sn); } catch (_) { /* shown via 401 */ }
}

$("aralik-kaydet").addEventListener("click", async () => {
  try {
    await api("POST", "/api/ayarlar", { ekran_araligi_sn: Number($("aralik").value) });
    $("aralik-durum").textContent = "Kaydedildi; bağlı makinelere hemen iletildi.";
  } catch (e) { $("aralik-durum").textContent = e.message; }
});

async function loadAudit() {
  try {
    const rows = await api("GET", "/api/denetim");
    $("denetim-satirlari").replaceChildren(...rows.map((r) => el("tr", {},
      el("td", {}, fmtTime(r.zaman)), el("td", {}, r.yonetici), el("td", {}, r.makine),
      el("td", {}, r.islem), el("td", {}, r.ayrinti), el("td", { class: r.sonuc === "tamam" ? "" : "hata" }, r.sonuc))));
  } catch (_) { /* 401 handled */ }
}

// ---- admins ----------------------------------------------------------------

async function loadAdmins() {
  try {
    const list = await api("GET", "/api/yoneticiler");
    $("yonetici-satirlari").replaceChildren(...list.map((a) => el("tr", {},
      el("td", {}, a.ad, a.sen ? el("span", { class: "rozet" }, "siz") : null),
      el("td", { class: "soluk kucuk" }, fmtTime(a.olusturma)),
      el("td", {}, a.sen ? null : el("button", { class: "tehlike", onclick: () => removeAdmin(a.ad) }, "Sil")))));
  } catch (_) { /* 401 handled */ }
}

async function removeAdmin(name) {
  if (!confirm(`${name} yöneticiliği silinsin mi? Açık oturumu hemen kapanır.`)) return;
  try { await api("POST", `/api/yoneticiler/${encodeURIComponent(name)}/sil`); loadAdmins(); }
  catch (e) { alert(e.message); }
}

/** QR step shared by first-run setup and adding an admin. */
function adminQr(box, name, r, confirmFn) {
  const msg = el("p", { class: "hata" });
  const code = el("input", { inputmode: "numeric", maxlength: 8, placeholder: "Telefondaki 6 haneli kod", autocomplete: "one-time-code" });
  const ok = el("button", { class: "birincil", onclick: async () => {
    msg.textContent = "";
    ok.disabled = true;
    try { await confirmFn(code.value.trim()); } catch (e) { msg.textContent = e.message; ok.disabled = false; }
  } }, "Onayla");
  box.replaceChildren(
    el("p", {}, `${name}, telefonunda Bacak Onay → QR tara ile bu kodu okutsun, ardından uygulamadaki kodu girin.`),
    el("img", { class: "qr", alt: "Bacak Onay QR kodu", src: r.qr }),
    el("p", { class: "soluk kucuk" }, "Kamera yoksa elle giriş anahtarı:"), el("div", { class: "kod-kutu" }, r.anahtar),
    el("p", { class: "uyari kucuk" }, "10 dakika içinde onaylanmazsa geçersiz olur."),
    el("label", {}, "Doğrulama kodu", code), msg, ok);
  code.focus();
}

$("yonetici-formu").addEventListener("submit", async (e) => {
  e.preventDefault();
  const f = new FormData(e.target);
  const name = f.get("ad").trim();
  $("yonetici-hata").textContent = "";
  try {
    const r = await api("POST", "/api/yoneticiler", { ad: name, parola: f.get("parola") });
    e.target.reset();
    const box = el("div");
    openDialog(header(`${name} — yeni yönetici`), box);
    adminQr(box, name, r, async (kod) => {
      await api("POST", "/api/yoneticiler/onayla", { ad: name, kod });
      box.replaceChildren(el("p", {}, `✓ ${name} artık panele giriş yapabilir.`));
      loadAdmins();
    });
  } catch (ex) { $("yonetici-hata").textContent = ex.message; }
});

// ---- first-run setup ---------------------------------------------------------

function showSetup() {
  $("giris").hidden = true;
  $("uygulama").hidden = true;
  $("kurulum").hidden = false;
  const box = $("kurulum-icerik");
  const token = new URLSearchParams(location.hash.slice(1)).get("kurulum");
  if (!token) {
    box.replaceChildren(el("p", { class: "hata" }, "Henüz yönetici yok. Paket kurulumunun yazdığı kurulum bağlantısıyla açın."),
      el("p", { class: "soluk kucuk" }, "Bağlantıyı sunucuda görmek için:"),
      el("div", { class: "kod-kutu" }, "sudo cat /var/lib/uzakyonetim-sunucu/kurulum-jetonu"),
      el("p", { class: "soluk kucuk" }, "ve adresin sonuna #kurulum=<jeton> ekleyin."));
    return;
  }
  const err = el("p", { class: "hata" });
  const form = el("form", { autocomplete: "off" },
    el("label", {}, "Kullanıcı adı", el("input", { name: "ad", required: true, pattern: "[a-z_][a-z0-9_\\-]{0,31}", placeholder: "mustafa" })),
    el("label", {}, "Parola (en az 12 karakter)", el("input", { name: "parola", type: "password", required: true, minlength: 12, autocomplete: "new-password" })),
    el("label", {}, "Parola (tekrar)", el("input", { name: "parola2", type: "password", required: true, autocomplete: "new-password" })),
    err, el("button", { class: "birincil", type: "submit" }, "Devam"));
  form.addEventListener("submit", async (e) => {
    e.preventDefault();
    const f = new FormData(form);
    const name = f.get("ad").trim();
    err.textContent = "";
    if (f.get("parola") !== f.get("parola2")) { err.textContent = "Parolalar eşleşmiyor."; return; }
    try {
      const r = await api("POST", "/api/kurulum", { jeton: token, ad: name, parola: f.get("parola") });
      adminQr(box, name, r, async (kod) => {
        const s = await api("POST", "/api/kurulum/onayla", { jeton: token, ad: name, kod });
        history.replaceState(null, "", location.pathname);
        csrf = s.csrf;
        showApp(s.yonetici);
      });
    } catch (ex) { err.textContent = ex.message; }
  });
  box.replaceChildren(form);
}

// ---- boot ------------------------------------------------------------------

(async () => {
  try {
    const r = await api("GET", "/api/oturum");
    csrf = r.csrf;
    showApp(r.yonetici);
    return;
  } catch (_) { /* not logged in */ }
  try {
    if ((await api("GET", "/api/kurulum")).gerekli) { showSetup(); return; }
  } catch (_) { /* fall back to login */ }
  showLogin();
})();
