// Command benchreport writes the report tables of a finished run: the Summary
// table, then Connections, BenchEcho and BenchMultiplex, each to its own .md
// file under output/report and to the console. It reads the JSON files
// benchcli-rust wrote, one per framework and benchmark.
//
// It takes the client's whole command line, as script/report.sh passes it on,
// and reads only its own flags out of it; see ownFlags.
package main

import (
	"flag"
	"os"
	"strings"

	"go-http3-benchmark/benchreport/report"
	"go-http3-benchmark/logging"
)

var (
	_          = flag.Bool("r", true, `make report; accepted for the client's command line, the report is all this does`)
	enableTPN  = flag.Bool("tpn", true, `whether the tables show the latency percentiles`)
	preffix    = flag.String("preffix", "", `report file preffix, e.g. "1m_connections_"`)
	suffix     = flag.String("suffix", "", `report file suffix, e.g. "_20060102150405"`)
	reportSort = flag.String("sort", report.DefaultSort, `report row order: "result" ranks the best result first, "framework" keeps the framework order`)
)

func main() {
	if err := flag.CommandLine.Parse(ownFlags(os.Args[1:])); err != nil {
		os.Exit(2)
	}
	if err := report.ValidateSort(*reportSort); err != nil {
		logging.Fatalf("%v", err)
	}
	report.Init(*enableTPN)

	sections := []struct{ name, data string }{
		{"Summary", report.GenerateSummary(*preffix, *suffix)},
		{"Connections", report.GenerateConnectionsReports(*preffix, *suffix, *enableTPN, *reportSort, nil)},
		{"BenchEcho", report.GenerateBenchEchoReports(*preffix, *suffix, *enableTPN, *reportSort, nil)},
		{report.BenchMultiplexName, report.GenerateBenchRateReports(*preffix, *suffix, *enableTPN, *reportSort, nil)},
	}
	for _, section := range sections {
		filename := report.Filename(section.name, *preffix, *suffix+".md")
		if err := report.WriteFile(filename, section.data); err != nil {
			logging.Printf("writing %v failed: %v", filename, err)
		}
		logging.Print(report.ConsoleSection(*preffix+section.name+*suffix, section.data))
	}
	logging.Print(logging.LongLine)
}

// ownFlags keeps the arguments that are this command's flags and drops the
// rest. The drivers hand the report step the same arguments they gave the
// client - -c, -en, -b and the rest - so that a -preffix or -suffix given once
// reaches both; the client defines those, and this command would otherwise
// stop at the first one with "flag provided but not defined". Every flag here
// is written -name=value, as the scripts write them, or -h for the usage.
func ownFlags(args []string) []string {
	var own []string
	for _, arg := range args {
		name := strings.TrimLeft(arg, "-")
		if name == arg {
			continue
		}
		name, _, _ = strings.Cut(name, "=")
		if flag.Lookup(name) != nil || name == "h" || name == "help" {
			own = append(own, arg)
		}
	}
	return own
}
