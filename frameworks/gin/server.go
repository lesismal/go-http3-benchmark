package main

import (
	"errors"
	"net"
	"net/http"
	"strconv"

	"go-http3-benchmark/config"
	"go-http3-benchmark/frameworks"
	"go-http3-benchmark/logging"

	"github.com/gin-gonic/gin"
	"github.com/quic-go/quic-go"
	"github.com/quic-go/quic-go/http3"
)

func main() {
	frameworks.Init(config.Gin)
	control := frameworks.StartControlServer()
	defer control.Close()

	// gin.New rather than gin.Default: no logger and no recovery middleware,
	// so the numbers are gin's router and context on top of quic-go rather
	// than a line of log per request.
	gin.SetMode(gin.ReleaseMode)
	router := gin.New()
	router.Any(config.EchoPath, onEcho)

	// gin's own HTTP/3 is RunQUIC, which is http3.ListenAndServeQUIC with
	// router.Handler(): one port, and quic-go's default limits. The servers
	// here are the same http3.Server with the same handler, set up as quicgo
	// sets up its own, one per port and with the shared -streams and -idle,
	// so that the difference between the two rows is gin and nothing else.
	tlsConfig := http3.ConfigureTLSConfig(frameworks.TLSConfig())
	quicConfig := &quic.Config{
		MaxIdleTimeout:     *frameworks.IdleTimeout,
		MaxIncomingStreams: int64(*frameworks.MaxStreams),
	}

	addrs := frameworks.ServerAddrs()
	var servers []*http3.Server
	for _, addr := range addrs {
		conn, err := net.ListenPacket("udp", addr)
		if err != nil {
			logging.Fatalf("listen %v failed: %v", addr, err)
		}
		server := &http3.Server{
			Handler:    router.Handler(),
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

func onEcho(c *gin.Context) {
	body, bufp, err := frameworks.ReadBody(c.Request.Body, c.Request.ContentLength)
	defer frameworks.BodyPool.Put(bufp)
	if err != nil {
		c.String(http.StatusBadRequest, err.Error())
		return
	}
	// Set, so that the response's HEADERS frame carries the length, as
	// quicgo's does.
	c.Header("Content-Length", strconv.Itoa(len(body)))
	c.Data(http.StatusOK, "application/octet-stream", body)
}
