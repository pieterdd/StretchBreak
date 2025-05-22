FROM fedora

RUN dnf update -y
RUN dnf install -y cargo
RUN dnf install -y pkgconf-pkg-config alsa-lib-devel dbus-devel libX11-devel libXScrnSaver-devel glib2-devel cairo-devel cairo-gobject-devel gtk4-devel libadwaita-devel
RUN cargo install cargo-generate-rpm

WORKDIR /build
COPY . /build
RUN cargo build --release
RUN cargo generate-rpm -o stretch-break.rpm
