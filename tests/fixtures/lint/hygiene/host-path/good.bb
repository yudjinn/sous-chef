SUMMARY = "Installs into the image directory"
LICENSE = "MIT"

do_configure() {
	./configure --prefix=/usr --sysconfdir=/etc
	/usr/bin/env true
}

do_install() {
	install -d ${D}${bindir} $D/usr/share/app
	install -m 0755 app ${D}${bindir}/app
	# never copy to /usr/bin on the host
	sed -i -e 's,/usr/lib,${libdir},' ${D}${bindir}/app
	cat > ${D}${sysconfdir}/app.conf <<EOF
plugins = /usr/lib/app
EOF
	echo done > /dev/null
}

do_install:append() {
	:
}

pkg_postinst:${PN}() {
	ln -sf /usr/bin/app /usr/bin/app-link
}

python do_report() {
    bb.plain("/usr/lib is fine in python")
}
