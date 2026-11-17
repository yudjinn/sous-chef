SUMMARY = "a2"
LICENSE = "MIT"

EXTRA_OEMAKE = "V=1"
CFLAGS += "-O2"
PACKAGECONFIG ??= "x"
INSANE_SKIP:${PN} = "ldflags"
FILES:${PN} += "/opt/tool"

do_install() {
    install -d ${D}/opt/tool
    install -m 0755 tool ${D}/opt/tool/
    # a comment the comparison ignores
    install -m 0644 tool.conf ${D}/opt/tool/
    ln -s tool ${D}/opt/tool/t
    chmod 0755 ${D}/opt/tool
}
