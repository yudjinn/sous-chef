SUMMARY = "Every download pinned"
SRC_URI = "https://example.org/releases/good-${PV}.tar.gz \
           git://example.org/lib.git;protocol=https;branch=main;name=lib \
           gitsm://example.org/mods.git;protocol=https;nobranch=1;rev=89abcdef0123456789abcdef0123456789abcdef;name=mods \
           ftp://example.org/extra.tar.xz;sha256sum=3333333333333333333333333333333333333333333333333333333333333333;name=extra \
           file://local.patch"
SRC_URI[sha256sum] = "1111111111111111111111111111111111111111111111111111111111111111"
SRCREV_lib = "0123456789abcdef0123456789abcdef01234567"
