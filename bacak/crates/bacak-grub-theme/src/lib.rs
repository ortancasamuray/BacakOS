// This crate is a packaging-only crate.
// It ships the "BacakOS" GRUB theme (files/theme.txt + background.png) to
// /boot/grub/themes/bacakos/ — no compiled code. The postinst script sets
// GRUB_DISTRIBUTOR="Bacak OS" and GRUB_THEME in /etc/default/grub, then runs
// update-grub so the boot menu shows "Bacak OS" with the desktop wallpaper
// as its background.
