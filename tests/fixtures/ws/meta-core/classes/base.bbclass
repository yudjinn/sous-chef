# Stand-in for base.bbclass: always inherited globally.

BB_DEFAULT_TASK ?= "build"

def base_set_filespath(path, d):
    filespath = []
    extrapaths = (d.getVar("FILESEXTRAPATHS") or "")
    for p in extrapaths.split(":") + path:
        if p and p != "__default":
            filespath.append(p)
    return ":".join(filespath)

addtask fetch
do_fetch[dirs] = "${DL_DIR}"
python base_do_fetch() {
    src_uri = (d.getVar('SRC_URI') or "").split()
    if not src_uri:
        return
    bb.fetch2.Fetch(src_uri, d).download()
}

addtask unpack after do_fetch
python base_do_unpack() {
    src_uri = (d.getVar('SRC_URI') or "").split()
    bb.fetch2.Fetch(src_uri, d).unpack(d.getVar('WORKDIR'))
}

addtask patch after do_unpack
base_do_patch() {
	:
}

addtask configure after do_patch
base_do_configure() {
	:
}

addtask compile after do_configure
base_do_compile() {
	if [ -e Makefile ]; then
		oe_runmake
	fi
}

addtask install after do_compile
do_install[dirs] = "${B}"
base_do_install() {
	:
}

addtask build after do_install
do_build[noexec] = "1"

EXPORT_FUNCTIONS do_fetch do_unpack do_patch do_configure do_compile do_install

addhandler base_eventhandler
base_eventhandler[eventmask] = "bb.event.ConfigParsed"
python base_eventhandler() {
    d.setVar('BB_VERSION', bb.__version__)
}
