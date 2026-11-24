#!/bin/sh

# CatTunnel server installer (fork of the TrustTunnel install script).
#
# Downloads a prebuilt release from github.com/Cattunnel/cattunnel-server,
# checks it against the release SHA256SUMS and unpacks it into the install
# directory. Running it again updates the binaries and leaves the settings
# (vpn.toml, hosts.toml, rules.toml, credentials.toml, certificates) alone.

set -e -u

repo='Cattunnel/cattunnel-server'
repo_url="https://github.com/${repo}"

# Files that hold the operator's settings. The installer never overwrites them.
keep_files='vpn.toml hosts.toml rules.toml credentials.toml'

# Function log is an echo wrapper that writes to stderr if the caller
# requested verbosity level greater than 0.  Otherwise, it does nothing.
log() {
  if [ "$verbose" -gt '0' ]
  then
    echo "$1" 1>&2
  fi
}

# Function error_exit is an echo wrapper that writes to stderr and stops the
# script execution with code 1.
error_exit() {
  echo "$1" 1>&2

  exit 1
}

# Function usage prints the note about how to use the script.
usage() {
  echo 'Usage: install.sh [-o output_dir] [-v] [-h] [-u] [-p archive_path] [-V version] [-a y|n]' 1>&2
}

# Function parse_opts parses the options list and validates it's combinations.
parse_opts() {
  while getopts 'vho:uV:p:a:' opt "$@"
  do
    case "$opt"
    in
    'v')
      verbose='1'
      ;;
    'h')
      usage
      exit 0
      ;;
    'o')
      output_dir="$OPTARG"
      ;;
    'u')
      uninstall='1'
      ;;
    'V')
      version="${OPTARG#v}"
      ;;
    'p')
      archive_path="$OPTARG"
      ;;
    'a')
      case "$OPTARG" in
      [yY]|[yY][eE][sS])
        auto_answer="y"
        ;;
      [nN]|[nN][oO])
        auto_answer="n"
        ;;
      *)
        echo "Invalid value for -a. Use 'y' or 'n' (or 'yes'/'no')." 1>&2
        exit 1
        ;;
      esac
      ;;
    *)
      usage
      exit 2
    ;;
    esac
  done
}

# Function is_little_endian checks if the CPU is little-endian.
#
# See https://serverfault.com/a/163493/267530.
is_little_endian() {
  is_little_endian_result="$(
    printf 'I'\
      | hexdump -o\
      | awk '{ print substr($2, 6, 1); exit; }'
  )"
  readonly is_little_endian_result

  [ "$is_little_endian_result" -eq '1' ]
}

# Function set_os sets the os if needed and validates the value.
set_os() {
  if [ "$os" = '' ]
  then
    os="$( uname -s )"
    case "$os"
    in
    ('Darwin')
      os='macos'
      ;;
    ('Linux')
      os='linux'
      ;;
    (*)
      error_exit "Unsupported operating system: '$os'"
      ;;
    esac
  fi

  case "$os"
  in
  ('linux')
    ;;
  ('macos')
    error_exit "There are no macOS builds of the CatTunnel server. Only linux-x86_64 and linux-aarch64 are released: ${repo_url}/releases"
    ;;
  (*)
    error_exit "Unsupported operating system: '$os'"
    ;;
  esac

  log "Operating system: $os"
}

# Function set_cpu sets the cpu if needed and validates the value.
set_cpu() {
  if [ "$cpu" = '' ]
  then
    cpu="$( uname -m )"
    case "$cpu"
    in
    ('x86_64'|'x86-64'|'x64'|'amd64')
      cpu='x86_64'
      ;;
    ('armv7l' | 'armv8l')
      cpu='armv7'
      ;;
    ('aarch64'|'arm64')
      cpu='aarch64'
      ;;
    ('mips')
      if is_little_endian
      then
        cpu="mipsel"
      fi
      ;;
    (*)
      error_exit "unsupported cpu type: $cpu"
      ;;
    esac
  fi

  case "$cpu"
  in
  ('x86_64'|'aarch64')
    ;;
  (*)
    error_exit "Unsupported cpu type: $cpu. Released: x86_64, aarch64"
    ;;
  esac

  log "CPU type: $cpu"
}

# Function for queries with support for auto-answer. Without a terminal
# (curl | sh in a container, CI) the answer is empty, which means "no".
ask_user() {
  printf "%s" "$1"
  if [ -n "$auto_answer" ]; then
    response="$auto_answer"
    echo "$response"
  else
    response=''
    read -r response < /dev/tty 2>/dev/null || echo
  fi
}

set_is_root() {
  if [ "$(id -u)" = "0" ]; then
    is_root='1'
  fi
}

# Function is_dir_owned_by_current_user checks if the output directory is owned by the current user
is_dir_owned_by_current_user() {
  dir="$1"

  if ! owner_name=$(stat -c '%U' "$dir"); then
    echo "Cannot stat '$dir'"
    return 0
  fi

  [ "$owner_name" = "$current_user" ]
}

# Function create_dir creates the output directory if it does not exist.
create_dir() {
  if mkdir -p "$output_dir" 2>/dev/null
  then
    log "'$output_dir' has been created"
    return 0
  fi

  if [ "$is_root" -eq 0 ] && type sudo > /dev/null 2>&1; then
    echo "Requesting sudo to create directory '$output_dir'"
    if sudo mkdir -p "$output_dir"; then
      sudo chown -R "${SUDO_USER:-$current_user}" "$output_dir"
      log "'$output_dir' has been created and ownership has been set to '${SUDO_USER:-$current_user}'"
    else
      error_exit "Failed to create '$output_dir' with sudo"
    fi
  else
    error_exit "Failed to create '$output_dir'. Run as root or choose another directory with -o DIR"
  fi
}

# Check if the directory is owned by the current user and change the ownership if needed
check_owner() {
  if [ "${is_root}" -eq 1 ]; then
    return 0
  fi
  if is_dir_owned_by_current_user "$output_dir";
  then
    log "'$output_dir' exists and is owned by '$current_user'"
  else
    log "'$output_dir' exists but is not owned by '$current_user'"
    ask_user "Would you like to change the ownership of $output_dir to $current_user? [y/N] "
    case "$response" in
    [yY]|[yY][eE][sS])
      target_user="${SUDO_USER:-$current_user}"
      if sudo chown -R "$target_user" "$output_dir"; then
        log "Ownership of $output_dir has been changed to '${target_user}'"
      else
        error_exit "Failed to change ownership of '$output_dir'"
      fi
      ;;
    *)
      error_exit "Installation cannot proceed without changing ownership of '$output_dir'"
      ;;
    esac
  fi
}

# Function check_out_dir requires the output directory to be set and exist.
check_out_dir() {
  if [ "$output_dir" = '' ]
  then
    output_dir='/opt'
  fi

  # If output_dir is '.'or '/opt', create inside it the 'trusttunnel' directory.
  # The name stays "trusttunnel": the systemd template and the running servers use it.
  if [ "$output_dir" = '.' ] || [ "$output_dir" = '/opt' ]
  then
    output_dir="${output_dir}/trusttunnel"
  fi

  if [ "$uninstall" -eq '1' ]
  then
    echo "CatTunnel server will be uninstalled from '$output_dir'"
    return 0
  else
    echo "CatTunnel server will be installed to '$output_dir'"
  fi

  set +e
  if [ ! -d "$output_dir" ];
  then
    log "'$output_dir' directory does not exist, attempting to create it..."
    create_dir
  else
    log "'$output_dir' directory exists"
    check_owner
  fi
  set -e
}

# Function cleanup removes the temporary directory.
cleanup() {
  if [ -n "$work_dir" ] && [ -d "$work_dir" ]; then
    rm -rf "$work_dir"
  fi
}

# Function sha256_of prints the SHA-256 of a file.
sha256_of() {
  if type sha256sum > /dev/null 2>&1; then
    sha256sum "$1" | awk '{ print $1 }'
  elif type shasum > /dev/null 2>&1; then
    shasum -a 256 "$1" | awk '{ print $1 }'
  else
    error_exit "Neither sha256sum nor shasum is available, cannot verify the package"
  fi
}

# Function resolve_version finds the latest release tag when -V was not given.
# The redirect of /releases/latest is used, so no API token and no rate limit.
resolve_version() {
  if [ -n "$version" ]; then
    return 0
  fi

  log "Looking up the latest release of ${repo}"
  latest_url="$(curl -fsSL -o /dev/null -w '%{url_effective}' "${repo_url}/releases/latest")" \
    || error_exit "Cannot find the latest release at ${repo_url}/releases/latest"
  version="${latest_url##*/}"
  version="${version#v}"
  case "$version" in
  ([0-9]*.[0-9]*)
    ;;
  (*)
    error_exit "Cannot read the version from '${latest_url}'. Pass it with -V"
    ;;
  esac
  echo "Latest version: $version"
}

# Main function.
configure() {
  if [ "$uninstall" -eq '1' ]
  then
    echo 'Uninstalling CatTunnel server...'
  else
    echo 'Installing CatTunnel server...'
  fi

  set_is_root
  set_os
  set_cpu
  check_out_dir

  if [ "$uninstall" -eq '1' ]; then
    return 0
  fi

  work_dir="$(mktemp -d)" || error_exit "Cannot create a temporary directory"
  trap cleanup EXIT

  if [ -n "$archive_path" ]; then
    [ -f "$archive_path" ] || error_exit "Archive '$archive_path' not found"
    pkg_name="$archive_path"
    need_download='0'
    log "Using user-provided archive: $archive_path"
    return 0
  fi

  type curl > /dev/null 2>&1 || error_exit "curl is required"
  resolve_version

  pkg_base="trusttunnel-v${version}-${os}-${cpu}.tar.gz"
  pkg_name="${work_dir}/${pkg_base}"
  url="${repo_url}/releases/download/v${version}/${pkg_base}"
  sums_url="${repo_url}/releases/download/v${version}/SHA256SUMS"

  log "Package name: '$pkg_base'"
}

# Function download downloads the package and SHA256SUMS.
download() {
  if [ "$uninstall" -eq '1' ] || [ "$need_download" -eq '0' ]; then
    return 0
  fi

  log "Downloading CatTunnel server package: $url"
  curl -fsSL "$url" -o "$pkg_name" \
    || error_exit "Failed to download $url"
  curl -fsSL "$sums_url" -o "${work_dir}/SHA256SUMS" \
    || error_exit "Failed to download $sums_url"
  sums_file="${work_dir}/SHA256SUMS"
  log "Package has been downloaded"
}

# Function verify_checksum compares the package with the SHA256SUMS line of the
# same name. There is no signature check: the release has no signatures, so this
# protects against a damaged download, not against a replaced release.
verify_checksum() {
  if [ "$uninstall" -eq '1' ]; then
    return 0
  fi

  if [ -z "$sums_file" ] && [ -n "$archive_path" ]; then
    sibling="$(dirname "$archive_path")/SHA256SUMS"
    if [ -f "$sibling" ]; then
      sums_file="$sibling"
    fi
  fi

  if [ -z "$sums_file" ]; then
    echo "WARNING: no SHA256SUMS next to '$archive_path', the archive is not verified" 1>&2
    return 0
  fi

  base="$(basename "$pkg_name")"
  expected="$(awk -v n="$base" '$2 == n || $2 == "*" n { print $1; exit }' "$sums_file")"
  if [ -z "$expected" ]; then
    error_exit "'$base' is not listed in $sums_file, refusing to install"
  fi
  actual="$(sha256_of "$pkg_name")"
  if [ "$expected" != "$actual" ]; then
    error_exit "SHA-256 mismatch for '$base': expected $expected, got $actual. Nothing was installed"
  fi
  log "SHA-256 matches: $actual"
}

# Function check_package checks that the archive can be read.
check_package() {
  if [ "$uninstall" -eq '1' ]; then
    return 0
  fi

  log "Checking package '$pkg_name'"
  if ! tar -f "$pkg_name" -z -t > /dev/null
  then
    error_exit "Error checking '$pkg_name'"
  fi
}

# Function is_kept tells whether a file name is one of the settings files.
is_kept() {
  for kept in $keep_files; do
    [ "$1" = "$kept" ] && return 0
  done
  return 1
}

# Function remove_existing removes program files of an old install. The settings
# and certificates in the directory are not touched.
remove_existing() {
  log 'Removing legacy program files...'
  for f in setup_wizard trusttunnel_endpoint_setup_wizard; do
    rm -f "${output_dir}/${f}"
  done
}

# Function handle_uninstall removes the program from the output directory.
handle_uninstall() {
  if [ ! -f "${output_dir}/trusttunnel_endpoint" ]
  then
    error_exit "CatTunnel server is not installed in '${output_dir}'. Please specify the correct installation directory"
  fi

  if pgrep -x "trusttunnel_endpoint" > /dev/null 2>&1
  then
    error_exit "The endpoint is currently running, please stop it before uninstalling"
  fi

  remove_existing
  rm -f "${output_dir}/trusttunnel_endpoint" "${output_dir}/trusttunnel.service.template" "${output_dir}/LICENSE"
  log "Program files have been removed from '${output_dir}'. Settings and certificates are left in place"

  if [ -z "$(ls -A "${output_dir}")" ]
  then
    rmdir "${output_dir}" 2>/dev/null || log "Cannot remove empty directory '${output_dir}'"
  fi

  if [ -e "${systemd_service_path}" ]
  then
    log "The systemd service ${systemd_service_path} is found. If you want to remove it, do it manually"
  fi
}

# Function write_service_template writes the systemd template if the archive has none.
write_service_template() {
  tpl="${output_dir}/trusttunnel.service.template"
  if [ -e "$tpl" ]; then
    return 0
  fi
  cat > "$tpl" <<EOF
# Service template for the CatTunnel endpoint
# Copy it to the systemd directory and enable:
#
# sudo cp trusttunnel.service.template /etc/systemd/system/trusttunnel.service
# sudo systemctl daemon-reload
# sudo systemctl enable --now trusttunnel

[Unit]
Description=CatTunnel endpoint
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
WorkingDirectory=${output_dir}
ExecStart=${output_dir}/trusttunnel_endpoint vpn.toml hosts.toml
Restart=on-failure
RestartSec=3

[Install]
WantedBy=multi-user.target
EOF
  log "Wrote ${tpl}"
}

# Function unpack unpacks the archive into a temporary directory and copies the
# files into the output directory one by one. A running binary is replaced by
# rename, so the old process keeps running until it is restarted. Settings
# files that already exist are kept.
unpack() {
  log "Unpacking '$pkg_name'"
  unpack_dir="${work_dir}/unpacked"
  mkdir -p "$unpack_dir"
  tar -C "$unpack_dir" -f "$pkg_name" -x -z \
    || error_exit "Cannot unpack '$pkg_name'"

  # The release archive has one top-level directory; accept a flat one too.
  src_dir="$unpack_dir"
  top_count="$(ls -A "$unpack_dir" | wc -l)"
  if [ "$top_count" -eq 1 ] && [ -d "${unpack_dir}/$(ls -A "$unpack_dir")" ]; then
    src_dir="${unpack_dir}/$(ls -A "$unpack_dir")"
  fi

  [ -f "${src_dir}/trusttunnel_endpoint" ] \
    || error_exit "The archive has no trusttunnel_endpoint, nothing was installed"
  if [ -f "${src_dir}/setup_wizard" ] || [ -f "${src_dir}/trusttunnel_endpoint_setup_wizard" ]; then
    error_exit "The archive contains a standalone setup wizard. Use the installer from the matching release."
  fi

  remove_existing

  for path in "$src_dir"/* "$src_dir"/.[!.]*; do
    [ -e "$path" ] || continue
    name="$(basename "$path")"
    if is_kept "$name" && [ -e "${output_dir}/${name}" ]; then
      log "Keeping existing '${name}'"
      continue
    fi
    if [ -d "$path" ] && [ -e "${output_dir}/${name}" ]; then
      log "Keeping existing directory '${name}'"
      continue
    fi
    if [ -d "$path" ]; then
      cp -R "$path" "${output_dir}/${name}"
    else
      cp -f "$path" "${output_dir}/.${name}.new.$$"
      mv -f "${output_dir}/.${name}.new.$$" "${output_dir}/${name}"
    fi
  done
  chmod 755 "${output_dir}/trusttunnel_endpoint"
  write_service_template
  log "Package has been unpacked successfully"
}

report_success() {
  echo
  echo "============================================================"
  echo "  CatTunnel server was installed to '${output_dir}'"
  if [ -x "${output_dir}/trusttunnel_endpoint" ]; then
    "${output_dir}/trusttunnel_endpoint" --version 2>/dev/null | sed 's/^/  /' || true
  fi
  echo "============================================================"
  echo
  if [ "$was_installed" -eq '1' ]; then
    echo "--- Updated ---"
    echo
    echo "Settings and certificates in '${output_dir}' were not changed."
    echo "A running endpoint still uses the old binary. Restart it when ready:"
    echo
    echo "    sudo systemctl restart trusttunnel"
    echo
    return 0
  fi
  echo "--- Continue setup ---"
  echo
  echo "Run the Setup Wizard to continue the endpoint setup:"
  echo
  echo "    cd ${output_dir}"
  echo "    sudo ./trusttunnel_endpoint --setup-wizard"
  echo
  echo "--- Configure systemd service ---"
  echo
  echo "The '${output_dir}/trusttunnel.service.template' template"
  echo "could be used to run a configured endpoint as a systemd service."
  echo
  echo "Once the endpoint setup is finished, run the following commands"
  echo "to configure a systemd service:"
  echo
  echo "    cd ${output_dir}/"
  echo "    sudo cp trusttunnel.service.template /etc/systemd/system/trusttunnel.service"
  echo "    sudo systemctl daemon-reload"
  echo "    sudo systemctl enable --now trusttunnel"
  echo
}

# Entrypoint

systemd_service_path='/etc/systemd/system/trusttunnel.service'
output_dir=''
verbose='1'
cpu=''
os=''
version=''
uninstall='0'
is_root='0'
archive_path=''
auto_answer=''
need_download='1'
work_dir=''
sums_file=''
response=''
was_installed='0'
current_user="${USER:-$(id -un)}"

parse_opts "$@"

configure

if [ "$uninstall" -eq '1' ]; then
  handle_uninstall
  exit 0
fi

download
verify_checksum
check_package

if [ -f "${output_dir}/trusttunnel_endpoint" ]; then
  was_installed='1'
  log "CatTunnel server is already present in '${output_dir}', updating"
fi

unpack
report_success
