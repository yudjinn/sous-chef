SUMMARY = "Installs onto the build host"
LICENSE = "MIT"

do_install() {
	install -d /usr/bin
	install -m 0755 app /usr/bin/app
	cp app.conf "/etc/app.conf"
}

do_compile:append() {
	cp /home/builder/blob.bin ${B}/
}
