package main

import (
	"errors"
	"net"
	"net/http"
	"strconv"

	"go-http3-benchmark/config"
	"go-http3-benchmark/frameworks"
	"go-http3-benchmark/logging"

	"github.com/quic-go/quic-go"
	"github.com/quic-go/quic-go/http3"
)

func main() {
	frameworks.Init(config.QuicGo)
	control := frameworks.StartControlServer()
	defer control.Close()

	mux := http.NewServeMux()
	mux.HandleFunc(config.EchoPath, onEcho)

	tlsConfig := http3.ConfigureTLSConfig(frameworks.TLSConfig())
	quicConfig := &quic.Config{
		MaxIdleTimeout:     *frameworks.IdleTimeout,
		MaxIncomingStreams: int64(*frameworks.MaxStreams),
	}

	// One http3.Server per port, all on one mux, as net/http serves one
	// listener per Serve call. Each has a UDP socket of its own, and quic-go
	// demultiplexes the connections on it by connection ID.
	addrs := frameworks.ServerAddrs()
	var servers []*http3.Server
	for _, addr := range addrs {
		conn, err := net.ListenPacket("udp", addr)
		if err != nil {
			logging.Fatalf("listen %v failed: %v", addr, err)
		}
		server := &http3.Server{
			Handler:    mux,
			TLSConfig:  tlsConfig,
			QUICConfig: quicConfig,
		}
		servers = append(servers, server)
		go func() {
			if err := server.Serve(conn); err != nil && !errors.Is(err, http.ErrServerClosed) &&
				!errors.Is(err, quic.ErrServerClosed) {
				logging.Printf("server exit: %v", err)
			}
		}()
	}
	frameworks.LogListening(addrs)

	frameworks.WaitSignal()
	for _, server := range servers {
		server.Close()
	}
}

func onEcho(w http.ResponseWriter, r *http.Request) {
	body, bufp, err := frameworks.ReadBody(r.Body, r.ContentLength)
	defer frameworks.BodyPool.Put(bufp)
	if err != nil {
		http.Error(w, err.Error(), http.StatusBadRequest)
		return
	}
	// Set, so that the response's HEADERS frame carries the length, as fib's
	// does, rather than leaving the client to find the end at the stream's FIN.
	header := w.Header()
	header["Content-Type"] = contentType
	header["Content-Length"] = []string{strconv.Itoa(len(body))}
	w.Write(body)
}

var contentType = []string{"application/octet-stream"}
