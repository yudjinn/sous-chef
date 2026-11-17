# Class written with legacy override syntax.
OLDCLASS_FLAGS ?= ""

do_configure_prepend() {
	echo "configuring ${PN}"
}

EXTRA_OECONF_append_class-native = " --disable-docs"
