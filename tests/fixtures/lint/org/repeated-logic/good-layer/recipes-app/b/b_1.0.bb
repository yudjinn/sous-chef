SUMMARY = "b"
LICENSE = "MIT"

do_install() {
    install -d ${D}/opt/tool
    install -m 0755 tool ${D}/opt/tool/
    install -m 0644 tool.conf ${D}/opt/tool/
    ln -s tool ${D}/opt/tool/t
}
