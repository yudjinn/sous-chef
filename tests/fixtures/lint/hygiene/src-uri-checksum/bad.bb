SUMMARY = "Downloads without checksums or revisions"
SRC_URI = "https://example.org/releases/bad-${PV}.tar.gz \
           git://example.org/lib.git;protocol=https;name=lib \
           file://local.patch"
SRC_URI += "http://example.org/data.zip;name=data"
SRC_URI[data.sha256sum] = "2222222222222222222222222222222222222222222222222222222222222222"
SRC_URI:append = " git://example.org/tools.git;protocol=https;name=tools"
SRCREV_tools = "0123456789abcdef0123456789abcdef01234567"
