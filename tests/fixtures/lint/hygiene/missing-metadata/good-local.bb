# Shaped after OE-core's connman-conf.bb: only files of the layer, no upstream, so no
# HOMEPAGE.
SUMMARY = "Connman config to setup wired interface on qemu machines"
LICENSE = "GPL-2.0-only"
LIC_FILES_CHKSUM = "file://${COMMON_LICENSE_DIR}/GPL-2.0-only;md5=801f80980d171dd6425610833a22dbe6"

SRC_URI = "file://main.conf \
           file://wired.config \
"
