#!/bin/sh
# Copyright © Advanced Micro Devices, Inc., or its affiliates.
#
# SPDX-License-Identifier: MIT
#
# Refuse to clobber a `rocm` or `rocmd` this package does not own.
#
# Same rule as the Debian preinst, expressed with rpm's ownership query. rpm
# tracks which package owns which file; a binary copied into /usr/bin by hand is
# owned by nobody, so it would be overwritten on install and deleted on
# uninstall. Refusing is recoverable in one command; deleting is not.
set -e

unowned=""
for binary in rocm rocmd; do
  path="/usr/bin/$binary"
  [ -e "$path" ] || continue
  if ! rpm -qf "$path" >/dev/null 2>&1; then
    unowned="$unowned $path"
  fi
done

if [ -n "$unowned" ]; then
  echo "ROCm App installs the ROCm command-line tool, but these files already" >&2
  echo "exist and no package owns them:" >&2
  for p in $unowned; do echo "  $p" >&2; done
  echo "" >&2
  echo "Installing would overwrite them, and uninstalling ROCm App would then" >&2
  echo "delete them. Move or remove them first, for example:" >&2
  for p in $unowned; do echo "  sudo mv $p $p.saved" >&2; done
  echo "" >&2
  echo "A ROCm CLI installed by install.sh into ~/.local/bin is not affected:" >&2
  echo "this only concerns /usr/bin." >&2
  exit 1
fi

exit 0
