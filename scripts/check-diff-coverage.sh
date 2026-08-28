#!/usr/bin/env bash
set -euo pipefail

if (($# != 2)); then
  echo "Usage: $0 LCOV_FILE BASE_REF" >&2
  exit 2
fi

repo_dir=${COVERAGE_REPO_DIR:-$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)}
lcov_file=$1
base_ref=$2
coverage_boundary='^crates/(ctl/src/main|daemon/src/(assets|database|live_location|main|notification))\.rs$'

[[ -r $lcov_file ]] || {
  echo "Coverage report is unreadable: $lcov_file" >&2
  exit 1
}
git -C "$repo_dir" rev-parse --verify --quiet "$base_ref^{commit}" >/dev/null || {
  echo "Coverage base is not a commit: $base_ref" >&2
  exit 1
}

declare -A executable_lines=()
declare -A covered_lines=()
current_file=

while IFS= read -r record; do
  case $record in
    SF:*)
      current_file=${record#SF:}
      current_file=${current_file#"$repo_dir/"}
      ;;
    DA:*)
      [[ -n $current_file ]] || continue
      payload=${record#DA:}
      line_number=${payload%%,*}
      count=${payload#*,}
      count=${count%%,*}
      key="$current_file:$line_number"
      executable_lines["$key"]=1
      if ((count > 0)); then
        covered_lines["$key"]=1
      fi
      ;;
  esac
done <"$lcov_file"

uncovered=()
changed_executable=0
current_file=
new_line=0

while IFS= read -r diff_line; do
  case $diff_line in
    "+++ b/"*)
      current_file=${diff_line#+++ b/}
      ;;
    "@@ "*)
      if [[ $diff_line =~ \+([0-9]+)(,([0-9]+))? ]]; then
        new_line=${BASH_REMATCH[1]}
      fi
      ;;
    +*)
      if [[ $diff_line != "+++ "* && -n $current_file ]]; then
        if [[ $current_file =~ $coverage_boundary ]]; then
          new_line=$((new_line + 1))
          continue
        fi
        key="$current_file:$new_line"
        if [[ -n ${executable_lines[$key]+present} ]]; then
          changed_executable=$((changed_executable + 1))
          if [[ -z ${covered_lines[$key]+present} ]]; then
            uncovered+=("$key")
          fi
        fi
        new_line=$((new_line + 1))
      fi
      ;;
    " "*)
      new_line=$((new_line + 1))
      ;;
    -*)
      ;;
  esac
done < <(git -C "$repo_dir" diff --no-color --ignore-all-space --unified=0 \
  --src-prefix=a/ --dst-prefix=b/ \
  "$base_ref" -- '*.rs')

if ((${#uncovered[@]})); then
  printf 'Changed executable Rust lines without coverage:\n' >&2
  printf '  %s\n' "${uncovered[@]}" >&2
  exit 1
fi

echo "Diff coverage passed: $changed_executable changed executable Rust lines are 100% covered."
