# Workspace description for tests/differential.rs and scripts/differential.sh
# (docs/differential.md, "How to run it"). Paths are relative to $SOUS_CHEF_LIVE.
# `init`: `setup` sources <build>/init-build-env (bitbake-setup); `oe:<dir>` sources
# <dir>/oe-init-build-env <build>. `path` is prepended to PATH for BitBake (a python).
# `recipes` and `probe` may repeat; the words accumulate.
name = wrynose
build = bitbake-builds/kiss-wrynose-dogbonedark/build
init = setup
dumps = runs/differential
recorded = differential
machine = dogbonedark
distro = kiss
recipes = curl sl linux-kiss u-boot-kiss kiss-image packagegroup-core-boot
recipes = zlib openssl busybox dropbear ncurses dbus systemd-boot util-linux
recipes = gmp-native nativesdk-zlib quilt-native expat glib-2.0 base-files
recipes = python3 bash opencsd virtual/kernel virtual/bootloader
probe = curl sl virtual/kernel
