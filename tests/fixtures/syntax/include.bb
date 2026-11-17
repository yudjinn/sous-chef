require hello.inc
require recipes-core/hello/${BPN}-common.inc
include optional.inc
include ${@bb.utils.contains('DISTRO_FEATURES', 'x11', 'x11.inc', '', d)}
