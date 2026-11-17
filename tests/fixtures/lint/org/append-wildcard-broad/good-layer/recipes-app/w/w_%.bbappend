FILESEXTRAPATHS:prepend := "${THISDIR}/${PN}:"
SRC_URI += "file://w.cfg"
EXTRA_OECONF += "--x"
