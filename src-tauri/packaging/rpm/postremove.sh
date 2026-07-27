#!/bin/sh
# Copyright © Advanced Micro Devices, Inc., or its affiliates.
#
# SPDX-License-Identifier: MIT
#
# Remove the app's own registration on uninstall.
#
# rpm already removes every file this package owns, including /usr/bin/rocm and
# /usr/bin/rocmd, and only those. Package ownership is the ownership metadata.
# What rpm does not know about is the per-user autostart entry, which lives
# under each user's home and would otherwise relaunch a binary that is gone.
set -e

# $1 is 0 on a real erase and 1 during an upgrade. An upgrade must keep the
# user's autostart choice.
[ "$1" = "0" ] || exit 0

for home in /home/* /root; do
  [ -d "$home" ] || continue
  entry="$home/.config/autostart/rocm-app.desktop"
  [ -e "$entry" ] && rm -f "$entry"
done

exit 0
