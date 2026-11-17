fakeroot do_bar() {
	chown root:root ${D}/file
}

fakeroot python do_baz() {
    import os
    os.chown(d.expand('${D}/file'), 0, 0)
}
