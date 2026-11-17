SUMMARY = "Pinned revisions"
SRC_URI = "git://example.org/x.git;protocol=https;branch=main"
# SRCREV = "${AUTOREV}" while developing, in local.conf
SRCREV = "0123456789abcdef0123456789abcdef01234567"
SRCREV_FORMAT = "meta_machine"
PV = "1.0+git${SRCPV}"
