# FILESEXTRAPATHS mistakes that have a fix.
FILESEXTRAPATHS:prepend := "${THISDIR}/files"
FILESEXTRAPATHS:prepend = "${THISDIR}/${PN}:"
FILESEXTRAPATHS:append := "${THISDIR}/late:"
FILESEXTRAPATHS:prepend = "${THISDIR}/more"
FILESEXTRAPATHS = "${THISDIR}/all"
