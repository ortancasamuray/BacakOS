# Bacak Onay

🌐 [Türkçe](README.tr.md) · **English**

Two-step login for BacakOS: the **Bacak Onay** Android app (TOTP/HOTP
authenticator) plus `pam_bacakonay.so`, which asks for the code on the login
screen of **Turan** (the Bacak Display Manager, `bacak-display-manager`).

```
 Phone (Bacak Onay)                       BacakOS computer
 ┌─────────────────────┐   QR (otpauth://)  ┌──────────────────────────────┐
 │ Android Keystore    │◄───────────────────│ sudo bacakonay kur           │
 │  └ wrap key         │   (once)           │  └ /var/lib/bacakonay/<user> │
 │ AES-GCM vault       │                    │      (root, 0600)            │
 │ TOTP (RFC 6238)     │  6-digit code      │ Turan login screen           │
 │  "123 456" ◔ 18 s   │──── user types ───►│  password → pam_unix         │
 └─────────────────────┘                    │  code     → pam_bacakonay    │
                                            └──────────────────────────────┘
```

No network: the phone and the computer "talk" once, through the enrollment QR;
afterwards both sides derive codes from the clock. The app has no internet
permission at all.

## Layout

```
android/                         Bacak Onay (Kotlin, Jetpack Compose, Material 3)
  …/bacakonay/domain/            pure Kotlin: models, TotpGenerator (RFC 4226/6238),
                                 otpauth:// parser, Base32, repository interface
  …/bacakonay/data/              CryptoManager (Keystore wrap key + AES-256-GCM),
                                 VaultStore/VaultManager, repository implementation
  …/bacakonay/ui/                lock (BiometricPrompt + CryptoObject), codes list
                                 with countdown ring, CameraX+ZXing QR scan, manual entry
  app/src/test/                  RFC vectors, URI parsing, vault codec
linux/                           Rust workspace
  crates/bacakonay-core/         OTP, base32, otpauth URI, enrollment store
  crates/bacakonay-cli/          `bacakonay` command (+ deb packaging)
  crates/pam-bacakonay/          pam_bacakonay.so (+ end-to-end tests against real libpam)
  pam/                           sample lines for other PAM services
```

The only Turan change is two lines in `turan/pam/bacak-display-manager`; the
greeter already presents PAM's follow-up question ("Bacak Onay kodu:") as its
own input step.

## Security model

**Phone**
- Secrets are AES-256-GCM encrypted in `filesDir/vault.bin`; the vault key is
  wrapped by a non-exportable Android Keystore key (StrongBox if present, else TEE).
- The wrap key requires user authentication **per use** and is unlocked via
  `BiometricPrompt.CryptoObject`, so the biometric (or, on Android 11+, PIN/pattern)
  check is cryptographically bound to decryption — not just a UI gate.
- Backgrounding the app locks the vault and scrubs decrypted secrets.
  `FLAG_SECURE` blocks screenshots, recordings, casting and recents previews.
  Backups are disabled (a restored vault couldn't be opened anyway).
- Copied codes are flagged sensitive on Android 13+ and cleared after 30 s.
- Enrolling a new fingerprint keeps the vault; **removing the screen lock**
  permanently deletes the Keystore key → accounts must be re-added.

**Computer**
- Enrollments live in `/var/lib/bacakonay/<user>`: root-owned, dir 0700, files
  0600. Ownership/permissions are checked on every read; a mismatched,
  symlinked or corrupt record **denies login** (fail closed).
- Users can't read or replace their own secret; enrollment needs `sudo`.
- A used code is consumed (`last_step`): it can't be replayed even within its
  30 s window. Verify-and-consume runs under `flock`.
- ±1 step (±30 s) of clock drift is accepted (`pencere=N` to change).
- A wrong code falls through to `pam_faillock authfail`, so brute force counts
  toward the same lockout as password guesses.
- Users without an enrollment are unaffected (`PAM_IGNORE`); the `zorunlu`
  option makes it mandatory.
- Without the package installed the Turan line does **not** break logins: a
  missing module returns `PAM_MODULE_UNKNOWN`, and `module_unknown=1` skips it
  (`linux/crates/pam-bacakonay/tests/pam_stack.rs` tests this with real libpam).

## Building

```sh
cd linux
cargo test                                   # RFC vectors + real-libpam tests
cargo build --release
cargo deb --no-build -p bacakonay-cli        # → target/debian/bacakonay_0.1.0-1_amd64.deb

cd android
./gradlew testDebugUnitTest
./gradlew assembleRelease                    # signed if keystore.properties exists
```

Android signing follows uzakel-android: a git-ignored
`android/keystore.properties` (`storeFile`, `storePassword`, `keyAlias`, `keyPassword`).

## End-to-end setup on BacakOS

1. **Install** `bacakonay` and a `bacak-display-manager` that carries the new PAM
   lines: `sudo apt install ./bacakonay_0.1.0-1_amd64.deb`. On an existing system
   `/etc/pam.d/bacak-display-manager` is a conffile and dpkg may ask; to add the
   lines by hand, use `linux/pam/bacak-display-manager.snippet` (right after
   `pam_unix.so` and its `authfail` line).
2. **Prepare the phone:** install Bacak Onay, open it, create the vault with
   fingerprint/PIN. The device needs a screen lock.
3. **Enroll** (in that user's session): `sudo bacakonay kur`. Scan the terminal
   QR with **QR tara**, then type the 6-digit code from the app back into the
   terminal. Only a matching code activates 2FA, so a bad scan can't lock you
   out. Options: `--algoritma SHA256`, `--hane 8`, or another user:
   `sudo bacakonay kur ayse`.
4. **Check** before logging out: `sudo bacakonay dogrula` (doesn't consume the
   code), `sudo bacakonay durum`.
5. **Log in:** password on Turan → "Bacak Onay kodu:" step → code from the phone.
6. **Undo:** `sudo bacakonay kaldir`. Lost phone: run `bacakonay kaldir <user>`
   as root from another admin account or recovery mode.

### Troubleshooting

| Symptom | Cause / fix |
|---|---|
| "Bacak Onay kodu geçersiz" | Phone clock automatic? Drift over ±30 s is rejected. A code can't be reused — wait for the next one. |
| No code prompt | `sudo bacakonay durum`; is the PAM line in `/etc/pam.d/bacak-display-manager`? |
| Correct code still denied | `journalctl -t bacak-display-manager \| grep bacakonay` — "güvensiz kayıt" means fix `/var/lib/bacakonay` perms (700 dir, 600 files, owner root). |
| App says the vault key is invalid | Screen lock was removed; create a new vault and re-pair with `sudo bacakonay kur --zorla`. |
| Locked out after many failures | `pam_faillock`: `sudo faillock --user <user> --reset`. |

## Next steps

- Move enrollment into the Control Center (using the compositor's existing QR
  renderer) so no terminal is needed.
- The same PAM line for the session lock screen.
- Encrypted export/import for moving to a new phone.
