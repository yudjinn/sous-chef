# No :remove: the item is simply left out, or optional through PACKAGECONFIG.
PACKAGECONFIG ??= "x11"
PACKAGECONFIG[gtk] = "--with-gtk,--without-gtk,gtk+3"
SRC_URI = "file://b.patch"
REMOVE_OLD_FILES = "1"
MY_LIST:append = " remove"
SRC_URI_remove = "legacy syntax is another rule's business"
