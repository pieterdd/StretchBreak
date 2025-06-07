# Stretch Break

![Stretch Break logo](meta/logo-color-128x128.png)

Stretch Break is a digital wellbeing tool that helps you take regular breaks. It is similar to Workrave and SafeEyes.

The application was written with GNOME and Linux in mind, but the code is mostly cross-platform. It is accompanied by a GNOME Shell extension that counts down to your next break.

<p align="center">
    <img src="docs/mainWindow.png" alt="Main window" /><br>
    <img src="docs/gnomeShellWidget.png" alt="GNOME Shell widget" />
</p>


## Install

Get the app on Flathub [here](https://flathub.org/apps/io.github.pieterdd.StretchBreak). GNOME users may want to install the [companion extension](https://extensions.gnome.org/extension/8231/stretch-break-companion/) that displays break status and provides settings access from a context menu.

For a manual install, run `cargo build --release`. You may need to install additional system-level build dependencies (see [Dockerfile](Dockerfile) for reference).
