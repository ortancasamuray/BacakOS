# Uzak Yönetim

🌐 [Türkçe](README.tr.md) · **English**

Manage BacakOS computers on different networks from one web panel:

- **User provisioning:** create an account on a remote machine and enroll
  **Bacak Onay** two-step login at the same time (QR shown in the panel, the
  person scans it with their phone).
- **Screen monitoring:** periodic screenshots (30 s – 1 h); while monitoring is
  on, the machine shows a visible **"İzleniyor"** (being monitored) badge.
- **Audit log:** who did what, when, on which machine.

Agents dial **out** to the server (mTLS on 8444), so the machines' networks need
no open ports; only the server's 8444 (agents) and 8443 (panel) must be reachable.

## Preventing unauthorised use

| Threat | Mitigation |
|---|---|
| Rogue machine joining | Enrollment only with a **single-use, expiring join code** from the panel. The agent generates its own key; the server takes only the CSR's public key and sets every certificate field itself (CN = machine id, `clientAuth` only, not a CA). Only the token's SHA-256 is stored. |
| Fake server / MITM | The join code carries the SHA-256 of the server CA; the agent trusts only that CA **even on first contact**. Mutual TLS afterwards. |
| Unauthorised panel login | Argon2id password **and** mandatory Bacak Onay code (no replay). 5 failures per IP or user → 15 min lock. Server-side sessions (30 min idle / 8 h max), `HttpOnly; Secure; SameSite=Strict` cookie, CSRF token on every state change, strict CSP. |
| Compromised server | The agent runs **no arbitrary commands** — only: list users, create account (never in `sudo`), Bacak Onay begin/confirm/remove, screenshot. `/etc/uzakyonetim/politika.toml` on each machine can switch any of these off. |
| Secret leakage | Bacak Onay secrets are generated **on the agent**, shown once as a QR, never stored on the server, and only activated after the phone returns a valid code. Passwords reach `chpasswd` via stdin, never argv. |
| Stolen machine | **"Filodan çıkar"** revokes its certificate and drops the connection immediately. |
| Covert monitoring | Visible "İzleniyor" badge drawn by the compositor while monitoring is on; systemd removes it whenever the agent stops. The badge is part of the screenshots too. |

The server runs as a dedicated `uzakyonetim` system user with
`ProtectSystem=strict`, an empty capability set and a syscall filter.

## Layout

```
crates/uzy-proto/    agent ↔ server protocol (length-prefixed JSON frames)
crates/uzy-sunucu/   web panel (axum, HTTPS) + agent gateway (rustls mTLS), CA (rcgen),
                     SQLite, sessions/CSRF, audit; end-to-end TLS security tests
crates/uzy-ajan/     enrollment, mTLS session, account/Bacak Onay ops (bacakonay-core),
                     screenshots via grim, monitoring flag
web/                 panel UI (framework-free JS, CSP-clean)
paket/               systemd units, policy file, deb maintainer scripts
```

The badge lives in `bacak-compositor` (drawn top-right while
`/run/uzakyonetim/izleniyor` exists).

## Setup

**Server** (reachable from all networks):

```sh
sudo apt install ./uzakyonetim-sunucu_0.1.0-1_amd64.deb
sudo -u uzakyonetim uzakyonetim-sunucu kurulum --adres manage.school.example --adres 203.0.113.7
sudo -u uzakyonetim uzakyonetim-sunucu yonetici-ekle principal    # password + Bacak Onay QR
sudo systemctl enable --now uzakyonetim-sunucu
```

Open 8443/tcp and 8444/tcp. The panel uses a certificate from the server's own
CA by default; set `panel_sertifika` / `panel_anahtar` in
`/var/lib/uzakyonetim-sunucu/ayar.json` for a public one (agents keep using the private CA).

**Each BacakOS machine:** panel → **+ Makine ekle** → copy the command, then

```sh
sudo apt install ./uzakyonetim-ajan_0.1.0-1_amd64.deb
sudo uzakyonetim-ajan kaydol uzy1.eyJ…
sudo systemctl enable --now uzakyonetim-ajan
```

## Build & test

```sh
cargo test
cargo build --release
cargo deb --no-build -p uzy-sunucu -o target/debian
cargo deb --no-build -p uzy-ajan   -o target/debian
```

## Known limits

- Screenshots capture the current seat0 session (or the login screen); multiple
  monitors end up in one image.
- Admin TOTP secrets are stored in the server DB (0600, `uzakyonetim` only) —
  encrypt the server's disk.
- No certificate rotation (agent certs last 5 years); re-enroll to renew.
