#!/usr/bin/env bash
# 安装或更新 vva 到最新 release。VVA_INSTALL_DIR 指定安装目录，默认 ~/.local/bin，Termux 使用 $PREFIX/bin。
set -euo pipefail

repo="jerrita/vva"
os=$(uname -s)
arch=$(uname -m)
if [ "$os" = Linux ] && {
  [ -n "${TERMUX_VERSION:-}" ] ||
    [ "${PREFIX:-}" = /data/data/com.termux/files/usr ] ||
    [ "$(uname -o 2>/dev/null || true)" = Android ]
}; then
  os=Android
fi

case "$os/$arch" in
  Android/aarch64 | Android/arm64) platform=android-arm64 ;;
  Linux/x86_64) platform=linux-x64 ;;
  Linux/aarch64 | Linux/arm64) platform=linux-arm64 ;;
  Linux/riscv64) platform=linux-riscv64 ;;
  Darwin/arm64) platform=macos-arm64 ;;
  *) echo "不支持的平台: $os/$arch" >&2; exit 1 ;;
esac

if [ "$platform" = android-arm64 ]; then
  dir="${VVA_INSTALL_DIR:-${PREFIX:-/data/data/com.termux/files/usr}/bin}"
else
  dir="${VVA_INSTALL_DIR:-$HOME/.local/bin}"
fi

# 经 releases/latest 重定向解析最新 tag，不占用 GitHub API 限额
latest=$(curl -fsSL --retry 3 -o /dev/null -w '%{url_effective}' "https://github.com/$repo/releases/latest")
tag=${latest##*/}
case "$tag" in
  v*) ;;
  *) echo "无法获取最新版本" >&2; exit 1 ;;
esac

current=$("$dir/vva" --version 2>/dev/null || true)
if [ "$current" = "vva ${tag#v}" ]; then
  echo "已是最新版本 $tag ($dir/vva)"
  exit 0
fi

name="vva-$tag-$platform"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

echo "下载 $tag ..."
curl -fsSL --retry 3 -o "$tmp/$name.tar.gz" "https://github.com/$repo/releases/download/$tag/$name.tar.gz"
tar xzf "$tmp/$name.tar.gz" -C "$tmp"
installed=$("$tmp/$name/vva" --version) || { echo "下载的二进制无法运行" >&2; exit 1; }
[ "$installed" = "vva ${tag#v}" ] || { echo "版本不符: 期望 $tag，实际 $installed" >&2; exit 1; }

mkdir -p "$dir"
# 先写入同目录临时文件再 rename，运行中的旧进程不受影响
install -m 755 "$tmp/$name/vva" "$dir/.vva.new"
mv -f "$dir/.vva.new" "$dir/vva"
if [ -n "${current#vva }" ]; then
  echo "已从 ${current#vva } 更新到 $tag: $dir/vva"
else
  echo "已安装 vva $tag 到 $dir/vva"
fi

case ":$PATH:" in
  *":$dir:"*) ;;
  *) echo "请将 $dir 加入 PATH" ;;
esac
