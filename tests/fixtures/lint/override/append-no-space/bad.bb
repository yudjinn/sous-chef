# :append / :prepend on space-separated lists, missing the space.
DEPENDS:append = "zlib"
SRC_URI:prepend = "file://first.patch"
IMAGE_INSTALL:append:qemux86 = "qemu-extra"
RDEPENDS:${PN}:append = "bash"
EXTRA_OECONF:append = "--enable-foo \
    --enable-bar"
