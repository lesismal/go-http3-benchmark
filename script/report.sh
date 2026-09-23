#!/bin/bash

# Writes the report tables of a finished run with benchreport, from the JSON
# files the client left in output/report. It takes the client's command line,
# as the drivers pass it on, and reads only -sort, -preffix, -suffix and -tpn
# out of it.
#
# Row order for the three report tables. BENCH_REPORT_SORT is set and
# documented in script/config.sh: "result" (the default) ranks the best result
# first, "framework" keeps config.FrameworkList's order.
#
# It is passed before "$@" on purpose, so that a -sort given on the command
# line still wins: benchreport takes the last value of a repeated flag. Running
# this script on its own leaves the variable unset and the flag off, which
# leaves benchreport its own default rather than a second copy of it here.
#
# Nothing else in a report depends on this - both orders carry the same rows
# and the same numbers - so re-running just this script is enough to read a
# finished benchmark the other way round:
#
#   bash script/report.sh -sort=framework
report_sort_flag=""
if [ -n "${BENCH_REPORT_SORT:-}" ]; then
    report_sort_flag="-sort=${BENCH_REPORT_SORT}"
fi

echo "generate report ..."
echo
./output/bin/bench.report ${report_sort_flag} "$@"
echo
echo "generate report done"
