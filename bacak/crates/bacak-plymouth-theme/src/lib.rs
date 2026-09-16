// This crate is a packaging-only crate.
// It ships the "bacakos" Plymouth boot theme (files/bacakos.{plymouth,script}
// + assets) to /usr/share/plymouth/themes/bacakos/ — no compiled code. The
// postinst script sets it as the default theme and refreshes the initramfs
// so the new splash takes effect on the next boot.
