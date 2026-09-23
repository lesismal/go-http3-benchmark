package frameworks

import (
	"crypto/ecdsa"
	"crypto/elliptic"
	"crypto/rand"
	"crypto/tls"
	"crypto/x509"
	"crypto/x509/pkix"
	"math/big"
	"net"
	"sync"
	"time"

	"go-http3-benchmark/logging"
)

var (
	tlsOnce   sync.Once
	tlsConfig *tls.Config
)

// TLSConfig is the TLS 1.3 configuration every server's QUIC handshakes use:
// one self-signed ECDSA P-256 certificate, made when the server starts. The
// client does not verify it, so there is nothing to distribute; what matters
// is that every server signs its handshakes with the same kind of key, since
// the signature is a real part of what a handshake costs the server.
//
// Each server adds the "h3" ALPN protocol itself, the way its own package
// asks for it to be added.
func TLSConfig() *tls.Config {
	tlsOnce.Do(func() {
		cert, err := selfSignedCertificate()
		if err != nil {
			logging.Fatalf("generating the TLS certificate failed: %v", err)
		}
		tlsConfig = &tls.Config{
			Certificates: []tls.Certificate{cert},
			MinVersion:   tls.VersionTLS13,
		}
	})
	return tlsConfig.Clone()
}

func selfSignedCertificate() (tls.Certificate, error) {
	key, err := ecdsa.GenerateKey(elliptic.P256(), rand.Reader)
	if err != nil {
		return tls.Certificate{}, err
	}
	serial, err := rand.Int(rand.Reader, new(big.Int).Lsh(big.NewInt(1), 128))
	if err != nil {
		return tls.Certificate{}, err
	}
	now := time.Now()
	template := &x509.Certificate{
		SerialNumber:          serial,
		Subject:               pkix.Name{CommonName: "go-http3-benchmark"},
		DNSNames:              []string{"localhost"},
		IPAddresses:           []net.IP{net.IPv4(127, 0, 0, 1), net.IPv6loopback},
		NotBefore:             now.Add(-time.Hour),
		NotAfter:              now.Add(365 * 24 * time.Hour),
		KeyUsage:              x509.KeyUsageDigitalSignature,
		ExtKeyUsage:           []x509.ExtKeyUsage{x509.ExtKeyUsageServerAuth},
		BasicConstraintsValid: true,
	}
	der, err := x509.CreateCertificate(rand.Reader, template, template, &key.PublicKey, key)
	if err != nil {
		return tls.Certificate{}, err
	}
	return tls.Certificate{Certificate: [][]byte{der}, PrivateKey: key}, nil
}
