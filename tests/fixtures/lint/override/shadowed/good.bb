SUMMARY = "Nothing overwritten for nothing"
LICENSE = "MIT"
HOMEPAGE = "https://example.org/a"
MY_FLAGS = "-O1"
MY_COPY := "${MY_FLAGS}"
MY_FLAGS = "-O2"
SECTION = "libs"
SECTION:qemux86 = "base"
DESCRIPTION ?= "default"
DESCRIPTION = "set"
EXTRA_OEMAKE = "A=1"
do_install() {
	:
}
do_install() {
	echo again
}
