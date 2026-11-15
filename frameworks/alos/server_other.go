//go:build !(linux && amd64)

package main

import (
	"go-http3-benchmark/config"
	"go-http3-benchmark/frameworks"
	"go-http3-benchmark/logging"
)

// alos-http builds on Linux amd64 alone: its server is an epoll loop with
// amd64 assembly in its parsers.
func main() {
	frameworks.Init(config.Alos)
	logging.Fatalf("%v server: github.com/guno1928/alos-http runs on linux/amd64 only", config.Alos)
}
