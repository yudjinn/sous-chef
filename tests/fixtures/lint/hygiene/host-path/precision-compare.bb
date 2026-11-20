# lint-precision: strings compared in a test are text, not host paths. Trimmed from
# meta-oe (kirkstone) phoronix-test-suite_10.8.2.bb:20-28 and opencv_4.5.5.bb:226-231
# (scarthgap opencv_4.9.0.bb:195-200 is the same).
do_install() {
    DESTDIR=${D} ./install-sh ${exec_prefix}

    if [ "${systemd_unitdir}" != "/usr/lib/systemd" ]; then
        install -d ${D}/${systemd_unitdir}/system/
        mv ${D}/usr/lib/systemd/system/* ${D}/${systemd_unitdir}/system/
        rm -rf ${D}/usr/lib/
    fi
}

do_install:append() {
    # Move Python files into correct library folder (for multilib build)
    if [ "$libdir" != "/usr/lib" -a -d ${D}/usr/lib ]; then
        mv ${D}/usr/lib/* ${D}/${libdir}/
        rm -rf ${D}/usr/lib
    fi
    # Still host paths: a file test and a file operand.
    if [ -d /usr/lib/python3 ]; then
        cp -r /usr/lib/python3 ${D}${libdir}
    fi
}
