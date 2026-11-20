# lint-precision: trimmed from OE-core (scarthgap) recipes-kernel/lttng/lttng-modules_2.13.12.bb
# lines 12-25 and 45-47. SRC_URI:class-devupstream replaces SRC_URI for that variant; the
# :append still applies there, a += would not.
SRC_URI = "https://lttng.org/files/${BPN}/${BPN}-${PV}.tar.bz2 \
           file://0009-Rename-genhd-wrapper-to-blkdev.patch \
           "

# Use :append here so that the patch is applied also when using devupstream
SRC_URI:append = " file://0001-src-Kbuild-change-missing-CONFIG_TRACEPOINTS-to-warn.patch \
                 "

SRC_URI[sha256sum] = "d85fcb66c7bd31003ab8735e8c77700e5e4f417b4c22fe1f20112cf435abad79"

# Still reported: DEPENDS has no variant in the file.
DEPENDS = "virtual/kernel"
DEPENDS:append = " bc-native"

BBCLASSEXTEND = "devupstream:target"
SRC_URI:class-devupstream = "git://git.lttng.org/lttng-modules;branch=stable-2.13;protocol=https"
SRCREV:class-devupstream = "7584cfc04914cb0842a986e9808686858b9c8630"
