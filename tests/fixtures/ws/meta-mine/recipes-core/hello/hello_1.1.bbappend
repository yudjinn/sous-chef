# Local changes to hello 1.1 only.
FILESEXTRAPATHS:prepend := "${THISDIR}/${PN}:"

SRC_URI:append = " file://mine.patch"

EXTRA_OECONF:append = " --with-mine"

do_install:append() {
	install -m 0644 ${S}/README ${D}${sysconfdir}/hello.readme
}
