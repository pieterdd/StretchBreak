FROM fedora AS base

RUN dnf update -y
RUN dnf install -y cargo
RUN dnf install -y pkgconf-pkg-config alsa-lib-devel dbus-devel libX11-devel libXScrnSaver-devel glib2-devel cairo-devel cairo-gobject-devel gtk4-devel libadwaita-devel libnotify-devel

WORKDIR /build
COPY . /build

FROM base AS debug
RUN cargo build

FROM base AS rpm
RUN cargo install cargo-generate-rpm
RUN cargo build --release
RUN cargo generate-rpm -o stretch-break.rpm
