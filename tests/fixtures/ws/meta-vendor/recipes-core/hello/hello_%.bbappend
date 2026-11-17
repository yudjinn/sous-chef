# Vendor changes to every version of hello.
FILESEXTRAPATHS:prepend := "${THISDIR}/${PN}:"

SRC_URI:append = " file://vendor.patch"
SRC_URI:append:boardx = " file://boardx.cfg"

EXTRA_OECONF:append:boardx = " --enable-boardx"
# Deliberate typo of "boardx": this override never matches (overrides show=dead).
EXTRA_OECONF:append:boradx = " --enable-typo"
