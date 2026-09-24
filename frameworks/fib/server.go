package main

import (
	stdhttp "net/http"
	"time"

	"go-http3-benchmark/config"
	"go-http3-benchmark/frameworks"
	"go-http3-benchmark/logging"

	fib "github.com/lesismal/fib/go"
	fibhttp "github.com/lesismal/fib/go/http"
	fibhttp3 "github.com/lesismal/fib/go/http3"
)

func main() {
	frameworks.Init(config.Fib)
	control := frameworks.StartControlServer()
	defer control.Close()

	addrs := frameworks.ServerAddrs()

	h3Config := fibhttp3.DefaultConfig()
	h3Config.TLSConfig = frameworks.TLSConfig()
	h3Config.MaxIdleTimeout = *frameworks.IdleTimeout
	h3Config.MaxConcurrentStreams = uint64(*frameworks.MaxStreams)
	// The datagram size the quiche server and benchcli-rust send, where fib
	// would otherwise keep to the 1200 bytes every path carries.
	h3Config.MaxDatagramSize = 1350
	handler := fibhttp3.NewHandlerWithConfig(h3Config, fibhttp.HandlerFunc(onRequest))

	// One UDP engine bound to every benchmark port. A server per port would
	// give each one its own event loop but also its own descriptor table,
	// buffer pools and task pool, none of which the ports have any reason not
	// to share.
	serverConfig := fib.DefaultConfig()
	serverConfig.Network = "udp"
	serverConfig.Addrs = addrs
	// The engine closes a UDP peer that has been silent this long, which has
	// to come after QUIC's own idle timeout rather than before it: fib's
	// documentation asks for the QUIC one to be the shorter.
	serverConfig.UDPIdleTimeout = *frameworks.IdleTimeout + 30*time.Second
	engine, err := fib.Bind(serverConfig, handler)
	if err != nil {
		logging.Fatalf("bind %d addresses failed: %v", len(addrs), err)
	}
	frameworks.LogListening(addrs)
	go func() {
		if err := engine.Run(); err != nil {
			logging.Printf("server exit: %v", err)
		}
	}()

	frameworks.WaitSignal()
	engine.Stop()
}

func onRequest(c *fibhttp.Context, r *stdhttp.Request) {
	if r.URL.Path != config.EchoPath {
		_ = c.Respond(stdhttp.StatusNotFound, "text/plain; charset=utf-8", []byte("404 page not found\n"))
		return
	}
	// fib's HTTP/3 server has read the body whole before the handler runs, so
	// this never waits on the network; and Respond copies it into the
	// response it sends, so the buffer can go back to the pool as soon as it
	// returns.
	body, bufp, err := frameworks.ReadBody(r.Body, r.ContentLength)
	defer frameworks.BodyPool.Put(bufp)
	if err != nil {
		_ = c.Respond(stdhttp.StatusBadRequest, "text/plain; charset=utf-8", []byte(err.Error()))
		return
	}
	_ = c.Respond(stdhttp.StatusOK, "application/octet-stream", body)
}
