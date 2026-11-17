python () {
    d.setVar('FROM_ANON', '1')
}

python __anonymous() {
    d.appendVar('FROM_ANON', ' 2')
}
