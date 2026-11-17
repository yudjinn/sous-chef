SUMMARY = "Example application using python and fakeroot"
LICENSE = "CLOSED"

DEPENDS = "libfoo"

SRC_URI = "file://app.c"

APP_GREETING = "${@'hi' if d.getVar('MACHINE') == 'boardx' else 'hello'}"
EXTRA_OEMAKE = "GREETING=${APP_GREETING}"

RDEPENDS:${PN} = "libfoo"

python () {
    if d.getVar('MACHINE') == 'boardx':
        d.appendVar('EXTRA_OEMAKE', ' BOARD=1')
}

def app_helper_name(d):
    return d.getVar('PN') + '-helper'

do_compile() {
	${CC} ${CFLAGS} ${LDFLAGS} -DGREETING='"${APP_GREETING}"' -o app ${WORKDIR}/app.c
}

fakeroot do_install() {
	install -d ${D}${bindir}
	install -m 0755 app ${D}${bindir}/app
	chown root:root ${D}${bindir}/app
}

python do_report() {
    bb.plain("%s %s built for %s" % (d.getVar('PN'), d.getVar('PV'), d.getVar('MACHINE')))
}
addtask report after do_compile before do_build
