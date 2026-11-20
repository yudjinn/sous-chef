# Workspace description for tests/differential.rs and scripts/differential.sh
# (docs/differential.md, "Other releases"; the workspace: scripts/release-env.sh).
# Paths are relative to $SOUS_CHEF_LIVE. Format: wrynose.ws.
name = kirkstone
build = releases/kirkstone/build
init = oe:releases/kirkstone/openembedded-core
path = releases/kirkstone/python-bin
dumps = runs/acc-releases/kirkstone
recorded = differential-kirkstone
machine = qemux86-64
distro = poky
# sous-chef.toml [build_host]: the dumps' BUILD_ARCH / BUILD_OS (build-host)
build_arch = x86_64
build_os = linux
# autotools, meson, cmake, plain make
recipes = curl zlib openssl ncurses bash util-linux glib-2.0 dbus expat
# python: the interpreter, setuptools / pep517 (flit) modules
recipes = python3 python3-jinja2 python3-packaging
# kernel, bootloader, image, packagegroup, providers
recipes = linux-yocto u-boot core-image-minimal packagegroup-core-boot virtual/kernel
# bbappends from meta-yocto (base-files only on scarthgap)
recipes = busybox base-files psplash
# -native recipe file, BBCLASSEXTEND variants
recipes = quilt-native gmp-native nativesdk-zlib
# meta-oe
recipes = htop nano jq fmt protobuf
probe = curl virtual/kernel
