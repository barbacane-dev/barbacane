#!/bin/sh
# Assembles /rootfs, the files of a scratch image around a binary that links
# glibc, in a Debian stage with libc6, libgcc-s1 and ca-certificates installed:
# the files of libc6 and libgcc-s1 with their dpkg records and os-release, so
# scanners match them against Debian's security tracker; the CA bundle; the
# users (root, and nonroot as 65532); nsswitch.conf; /tmp. The non-root user's
# home is /home-nonroot, for `COPY --chown`. Fails if the binary does not start
# on these files alone.
#
# Usage: scratch-rootfs.sh <binary> <path in the image>
set -eu

binary=$1
path=$2
# The Debian packages the image carries.
set -- libc6 libgcc-s1

# The loader is found through /lib or /lib64, which Debian links into /usr.
for dir in lib lib32 lib64 libx32; do
    if [ -L "/$dir" ]; then
        link=$(readlink "/$dir")
        mkdir -p "/rootfs/$link"
        ln -s "$link" "/rootfs/$dir"
    fi
done

# The packages' files and links. Documentation is left out, copyright files
# kept; directories come with the files.
dpkg -L "$@" > /tmp/rootfs-listed
sort -u /tmp/rootfs-listed | while read -r file; do
    [ -d "$file" ] && continue
    [ -e "$file" ] || [ -L "$file" ] || continue
    case $file in
        /usr/share/doc/*/copyright) ;;
        /usr/share/doc/* | /usr/share/lintian/* | /usr/share/man/* | /usr/share/info/*) continue ;;
    esac
    printf '%s\n' "${file#/}"
done > /tmp/rootfs-files
tar -C / -cf /tmp/rootfs.tar --no-recursion -T /tmp/rootfs-files
tar -C /rootfs -xpf /tmp/rootfs.tar

mkdir -p /rootfs/var/lib/dpkg /rootfs/usr/lib /rootfs/etc/ssl/certs /rootfs/tmp /home-nonroot
dpkg-query --status "$@" > /rootfs/var/lib/dpkg/status
cp /usr/lib/os-release /rootfs/usr/lib/os-release
ln -s ../usr/lib/os-release /rootfs/etc/os-release

cp /etc/ssl/certs/ca-certificates.crt /rootfs/etc/ssl/certs/
printf 'root:x:0:0:root:/root:/sbin/nologin\nnonroot:x:65532:65532:nonroot:/home/nonroot:/sbin/nologin\n' \
    > /rootfs/etc/passwd
printf 'root:x:0:\nnonroot:x:65532:\n' > /rootfs/etc/group
# Without this file glibc asks DNS before /etc/hosts.
printf 'passwd: files\ngroup: files\nhosts: files dns\n' > /rootfs/etc/nsswitch.conf
chmod 1777 /rootfs/tmp

mkdir -p "/rootfs$(dirname "$path")"
cp "$binary" "/rootfs$path"
chroot /rootfs "$path" --version
