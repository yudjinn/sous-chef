python do_report() {
    import os
    bb.plain("PN is %s" % d.getVar('PN'))
    if True:
        pass
}

python do_report:append() {
    bb.plain("appended")
}
