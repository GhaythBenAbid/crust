#!/usr/bin/env bash
# Differential tests: run each case through curl and crust, compare stdout
# (and files written) plus exit code. Starts its own test server.
#
#   tests/compare.sh            run all cases
#   tests/compare.sh cookie     run cases whose name matches "cookie"
#
# Env: CURL (reference, default /usr/bin/curl), CRUST (default target/release/crust)
# CRUST_HTTP_PORT, CRUST_HTTPS_PORT and CRUST_PROXY_PORT default to free ports.
set -uo pipefail
cd "$(dirname "$0")/.."

CURL=${CURL:-/usr/bin/curl}
CRUST=${CRUST:-$PWD/target/release/crust}
FILTER=${1:-}
WORK=$(mktemp -d "${TMPDIR:-/tmp}/crust-compare.XXXXXX") || exit 1
source testserver/start.sh
trap 'stop_testserver; rm -rf "$WORK"' EXIT
start_testserver "$WORK" || exit 1

H=http://127.0.0.1:$HTTP_PORT
S=https://127.0.0.1:$HTTPS_PORT
P=http://127.0.0.1:$PROXY_PORT
export H S P WORK
printf 'line1\nline2\r\n' >"$WORK/data.txt"
printf 'binary\0data' >"$WORK/bin.dat"
printf '{"a":1}' >"$WORK/body.json"
printf 'X-From-File: yes\nX-Second: 2\n' >"$WORK/headers.txt"
printf 'machine 127.0.0.1 login nu password np\n' >"$WORK/netrc"
printf '127.0.0.1\tFALSE\t/\tFALSE\t0\tjarcookie\tfromjar\n' >"$WORK/jar.txt"

pass=0 fail=0 failed=()

# case NAME ARGS...   — ARGS are passed through `eval` so quoting works;
# files written under $WORK/out are compared too. Use `-A t` where the
# default User-Agent would differ.
case_() {
  local name=$1; shift
  [[ -n $FILTER && $name != *$FILTER* ]] && return
  # ENV_name tokens become "name=$P" (proxy) environment assignments.
  local envs="" rest=()
  for a in "$@"; do
    if [[ $a == ENV_* ]]; then envs+="${a#ENV_}=\$P "; else rest+=("$a"); fi
  done
  local args="${rest[*]}"
  for tool in curl crust; do
    local bin=$CURL; [[ $tool == crust ]] && bin=$CRUST
    local d="$WORK/$tool"; rm -rf "$d"; mkdir -p "$d/out"
    (cd "$d/out" && eval "$envs\"$bin\" $args" >"$d/stdout" 2>"$d/stderr" </dev/null; echo $? >"$d/rc")
    # normalize: tool names and timing-dependent values
    sed -e 's/curl: /TOOL: /; s/crust: /TOOL: /' "$d/stderr" >"$d/stderr.n" && mv "$d/stderr.n" "$d/stderr"
  done
  local diffs=""
  cmp -s "$WORK/curl/rc" "$WORK/crust/rc" || diffs+=" exit($(cat "$WORK/curl/rc") vs $(cat "$WORK/crust/rc"))"
  cmp -s "$WORK/curl/stdout" "$WORK/crust/stdout" || diffs+=" stdout"
  diff -r "$WORK/curl/out" "$WORK/crust/out" >/dev/null 2>&1 || diffs+=" files"
  if [[ -z $diffs ]]; then
    pass=$((pass + 1))
  else
    fail=$((fail + 1)); failed+=("$name")
    echo "FAIL $name:$diffs"
    echo "     args: $args"
    if [[ $diffs == *stdout* ]]; then
      diff <(head -c 2000 "$WORK/curl/stdout") <(head -c 2000 "$WORK/crust/stdout") | head -20 | sed 's/^/     /'
    fi
    if [[ $diffs == *files* ]]; then diff -r "$WORK/curl/out" "$WORK/crust/out" 2>&1 | head -10 | sed 's/^/     /'; fi
    if [[ $diffs == *exit* ]]; then
      echo "     curl stderr:  $(head -c 300 "$WORK/curl/stderr" | tr '\n' ' ')"
      echo "     crust stderr: $(head -c 300 "$WORK/crust/stderr" | tr '\n' ' ')"
    fi
  fi
}

# ---- basics
case_ get_small            -s '$H/small'
case_ get_https_k          -sk '$S/small'
case_ get_noscheme         -s '${H#http://}/small'
case_ echo_get             -s -A t '$H/echo'
case_ echo_query           -s -A t "\"\$H/echo/a/b?x=1&y=2\""
case_ head                 -sI '$H/small' '|' grep -iv '^date:'
case_ include              -si '$H/small' '|' grep -iv '^date:'
case_ binary_to_file       -s -o b.bin '$H/binary'
case_ big_to_file          -s -o big.bin "\"\$H/bytes?n=3000000\""
case_ status_404           -s '$H/status/404'
case_ status_204           -s '$H/status/204'
case_ status_304           -s '$H/status/304'
case_ fail_404             -sf '$H/status/404'
case_ fail_200             -sf '$H/small'
case_ fail_with_body       -s --fail-with-body '$H/status/500'
case_ http10_close_delim   -s '$H/http10'
case_ conn_close           -s '$H/close' '$H/small'
case_ chunked              -s "\"\$H/chunked?n=50&size=1000\""
case_ chunked_tiny         -s "\"\$H/chunked?n=300&size=1\""
case_ empty_body           -s '$H/empty'
case_ empty_to_file        -s -o empty.out '$H/empty'
case_ empty_truncates_file -s -o empty.out '$H/small' -o empty.out '$H/empty'
case_ empty_missing_dir    -s -o missing/empty.out '$H/empty'
case_ empty_http_failure   -sf -o empty.out '$H/status/404'
case_ empty_connect_failure -s -o empty.out 'http://127.0.0.1:1/'
case_ many_headers         -si '$H/many-headers' '|' grep -iv '^date:'
case_ https_big            -sk -o big.bin "\"\$S/bytes?n=5000000\""
case_ https_chunked        -sk "\"\$S/chunked?n=20&size=5000\""

# ---- methods and data
case_ post_data            -s -A t -d 'a=1' -d 'b=2' '$H/echo'
case_ post_data_file       -s -A t -d '@$WORK/data.txt' '$H/echo'
case_ post_data_binary     -s -A t --data-binary '@$WORK/data.txt' '$H/echo'
case_ post_data_raw        -s -A t --data-raw '@notafile' '$H/echo'
case_ data_urlencode       -s -A t --data-urlencode "'q=a b&c'" --data-urlencode '=x/y' '$H/echo'
case_ data_urlencode_file  -s -A t --data-urlencode 'f@$WORK/data.txt' '$H/echo'
case_ get_with_data        -s -A t -G -d 'q=1' -d 'r=2' '$H/echo'
case_ put_data             -s -A t -X PUT -d 'x' '$H/echo'
case_ delete               -s -A t -X DELETE '$H/echo'
case_ json                 -s -A t --json "'{\"k\":\"v\"}'" '$H/echo'
case_ json_file            -s -A t --json '@$WORK/body.json' '$H/echo'
case_ form                 -s -A t -F 'name=value' -F 'up=@$WORK/data.txt' '$H/echo'
case_ form_type_filename   -s -A t -F "'f=@\$WORK/bin.dat;type=application/x-test;filename=renamed.bin'" '$H/echo'
case_ form_string          -s -A t --form-string "'s=@literal'" '$H/echo'
case_ form_from_file       -s -A t -F "'v=<\$WORK/data.txt'" '$H/echo'
case_ upload               -s -A t -T '$WORK/data.txt' '$H/echo/up'
case_ upload_slash         -s -A t -T '$WORK/data.txt' '$H/echo/'

# ---- headers / auth
case_ custom_header        -s -A t -H "'X-A: 1'" -H "'X-B: two words'" '$H/echo'
case_ remove_header        -s -A t -H "'Accept:'" '$H/echo'
case_ empty_header         -s -A t -H "'X-Empty;'" '$H/echo'
case_ replace_host         -s -A t -H "'Host: example.test'" '$H/echo'
case_ headers_from_file    -s -A t -H '@$WORK/headers.txt' '$H/echo'
case_ user_agent           -s -A "'my agent/1.0'" '$H/echo'
case_ referer              -s -A t -e 'http://ref.test/' '$H/echo'
case_ basic_auth           -s -u user:pass '$H/basic-auth/user/pass'
case_ basic_auth_bad       -s -u user:nope '$H/basic-auth/user/pass'
case_ basic_auth_url       -s 'http://user:pass@${H#http://}/basic-auth/user/pass'
case_ bearer               -s --oauth2-bearer tok123 '$H/bearer'
case_ netrc                -s --netrc-file '$WORK/netrc' '$H/basic-auth/nu/np'
case_ range                -s -A t -r 0-99 '$H/echo'
case_ http10_request       -s -A t -0 '$H/echo'

# ---- redirects
case_ redirect_nofollow    -s '$H/redirect/3'
case_ redirect_follow      -sL '$H/redirect/3'
case_ redirect_abs         -sL '$H/abs-redirect/3'
case_ redirect_rel         -sL -A t '$H/rel-redirect'
case_ redirect_max         -sL --max-redirs 2 '$H/redirect/5'
case_ redirect_include     -sLi '$H/redirect/2' '|' grep -iv '^date:'
case_ redirect_302_post    -sL -A t -d x "\"\$H/r/302\""
case_ redirect_303_post    -sL -A t -d x "\"\$H/r/303\""
case_ redirect_307_post    -sL -A t -d x "\"\$H/r/307\""
case_ redirect_308_post    -sL -A t -d x "\"\$H/r/308\""
case_ redirect_custom_x    -sL -A t -X PUT -d x "\"\$H/r/302\""
case_ redirect_custom_301  -sL -A t -X PUT -d x '$H/r/301'
case_ redirect_custom_303  -sL -A t -X PUT -d x '$H/r/303'
case_ redirect_custom_307  -sL -A t -X PUT -d x '$H/r/307'
case_ redirect_custom_308  -sL -A t -X PUT -d x '$H/r/308'
case_ redirect_custom_type -sL -A t -X PUT -d x -H "'Content-Type: application/json'" '$H/r/302'
case_ redirect_custom_chain -sL -A t -X PUT -d x '"$H/r/302?to=/r/307"'
case_ redirect_https       -skL '"$H/r/302?to=$S/small"'
case_ redirect_auth_drop   -sL -A t -u a:b -H "'X-Keep: 1'" '"$H/r/302?to=http://localhost:$HTTP_PORT/echo"'
case_ redirect_head        -sIL '$H/redirect/2' '|' grep -iv '^date:'
case_ url_effective        -sL -o /dev/null -w "'%{url_effective} %{num_redirects} %{http_code}\n'" '$H/redirect/2'

# ---- output options
case_ output_O             -sO '$H/small'
case_ output_O_J           -sOJ '$H/disposition'
case_ remote_name_all      -s --remote-name-all '$H/small' "\"\$H/echo/x?y=1\""
case_ output_glob_subst    -s "\"\$H/echo/{a,b}/[1-2]\"" -o "'file_#1_#2.txt'" -A t
case_ create_dirs          -s --create-dirs -o 'sub/dir/f.txt' '$H/small'
case_ dump_header          -s -D hdr.txt -o body.txt '$H/small' '&&' grep -ic content-length hdr.txt
case_ dump_header_stdout   -s -D - '$H/small' '|' grep -iv '^date:'
case_ write_out            -s -o /dev/null -w "'%{http_code} %{size_download} %{content_type} %{method} %{scheme} %{num_redirects}\n'" '$H/small'
case_ write_out_header     -s -o /dev/null -w "'%header{content-type}|%{response_code}\n'" '$H/small'
case_ write_out_err        -s -w "'%{http_code} %{exitcode}\n'" 'http://127.0.0.1:1/'
case_ write_out_stderr     -s -o /dev/null -w "'%{stderr}to-stderr%{stdout}to-stdout\n'" '$H/small'
case_ multi_url            -s '$H/small' '$H/echo/x' -A t
case_ glob_range           -s "\"\$H/echo/[1-3]\"" -A t
case_ glob_set             -s "\"\$H/echo/{a,b,c}\"" -A t
case_ globoff              -sg -A t "\"\$H/echo/[1-2]\""
case_ binary_terminal_safe -s '$H/binary' '|' wc -c

# ---- encoding
case_ compressed_gzip      -s --compressed '$H/gzip'
case_ compressed_deflate   -s --compressed '$H/deflate'
case_ no_compressed_gzip   -s '$H/gzip' '|' wc -c
case_ url_space            -s -A t "\"\$H/echo/with space?q=a b\""
case_ url_dotsegments      -s -A t "\"\$H/echo/a/../b/./c\""
case_ path_as_is           -s -A t --path-as-is "\"\$H/echo/a/../b\""

# ---- cookies
case_ cookie_literal       -s -b 'a=1; b=2' '$H/cookies'
case_ cookie_engine_follow -sL -b '' "\"\$H/cookies/set?x=1&y=2\""
case_ cookie_nofollow      -sL "\"\$H/cookies/set?x=1\""
case_ cookie_jar_write     -s -c jar.out "\"\$H/cookies/set?k=v\"" '&&' grep -c k jar.out
case_ cookie_jar_read      -s -b '$WORK/jar.txt' '$H/cookies'

# ---- proxy
case_ proxy_flag_http      -s -x '$P' '$H/small' -i '|' grep -iE "'^(x-via|hello)'"
case_ proxy_env_http       -s '$H/small' -i '|' grep -iE "'^(x-via|hello)'" ENV_http_proxy
case_ proxy_connect_https  -sk -x '$P' '$S/small'
case_ proxy_noproxy        -s -x '$P' --noproxy 127.0.0.1 -i '$H/small' '|' grep -ic x-via
case_ proxy_bad            -s -x 'http://127.0.0.1:1' '$H/small'

# ---- errors / timeouts / retry
case_ err_resolve          -s 'http://nonexistent.invalid/'
case_ err_refused          -s 'http://127.0.0.1:1/'
case_ err_refused_S        -sS 'http://127.0.0.1:1/'
case_ err_protocol         -s 'gopherx://x/'
case_ err_tls_selfsigned   -s '$S/small'
case_ cacert               -s --cacert '$WORK/cert.pem' '$S/small'
case_ max_time_hit         -s -m 0.3 '$H/stall'
case_ max_time_ok          -s -m 5 "\"\$H/slow?ms=50\""
case_ max_time_zero        -s -m 0 "\"\$H/slow?ms=80\""
case_ max_time_reset       -s -m 0.02 -m 0 --connect-timeout 0.02 --connect-timeout 0 "\"\$H/slow?ms=80\""
case_ connect_timeout_zero -s --connect-timeout 0 '$H/small'
case_ partial_file         -s -m 1 '$H/stall' '|' wc -c
case_ retry_success        -s --retry 3 --retry-delay 0 "\"\$H/flaky?key=k1&fail=2\""
case_ retry_exhausted      -s --retry 1 --retry-delay 0 "\"\$H/flaky?key=k2&fail=5\""
case_ retry_fail_flag      -sf --retry 2 --retry-delay 0 "\"\$H/flaky?key=k3&fail=1\""
case_ resolve              -s -A t --resolve 'myhost.test:$HTTP_PORT:127.0.0.1' 'http://myhost.test:$HTTP_PORT/echo'
case_ happy_eyeballs       -s -m 5 --resolve 'hosty.test:$HTTP_PORT:10.255.255.1,127.0.0.1' 'http://hosty.test:$HTTP_PORT/small'
case_ ipv4_only            -s -4 'http://localhost:$HTTP_PORT/small'
case_ unknown_option       -s --definitely-not-an-option '$H/small'
case_ no_url               -s

echo
echo "passed $pass, failed $fail"
[[ $fail -eq 0 ]]
