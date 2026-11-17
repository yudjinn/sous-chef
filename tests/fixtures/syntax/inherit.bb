inherit autotools
inherit pkgconfig gettext
inherit ${BUILD_SYSTEM_CLASS}
inherit_defer ${@'native' if d.getVar('IS_NATIVE') == '1' else ''}
inherit_defer python3native
