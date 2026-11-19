// Test + benchmark server for crust.
//
// HTTP on :8080, HTTPS (self-signed, h1 + h2) on :8443, forward proxy on :8081.
// CRUST_HTTP_PORT, CRUST_HTTPS_PORT and CRUST_PROXY_PORT override the defaults.
// Use 0 to allocate a free port. The first stdout line lists the bound ports.
// Endpoints (on both HTTP and HTTPS):
//
//	/small                 13-byte body
//	/bytes?n=N             N zero bytes, Content-Length
//	/binary                small body containing NUL bytes
//	/echo[/...]            request echo: method, path, sorted headers (minus
//	                       User-Agent), then body; multipart bodies are listed
//	                       part by part so random boundaries don't matter
//	/chunked?n=N&size=S    N chunks of S bytes ('a'..), Transfer-Encoding: chunked
//	/status/CODE           responds with CODE
//	/redirect/N            302 chain down to /redirect/0
//	/abs-redirect/N        same, absolute Location URLs
//	/rel-redirect          302 with relative Location "../echo/rel"
//	/r/CODE?to=URL         redirect with the given code to URL (default /echo)
//	/basic-auth/U/P        200 if Basic credentials match, else 401
//	/bearer                echoes the Authorization header
//	/gzip /deflate         compressed text bodies
//	/cookies/set?k=v&...   sets cookies then 302 to /cookies
//	/cookies               lists cookies received
//	/slow?ms=N             sleeps N ms before replying
//	/stall                 sends headers + part of the body, then stalls
//	/http10                raw HTTP/1.0 response, close-delimited body
//	/close                 Connection: close
//	/empty                 200 with zero-length body
//	/disposition           Content-Disposition: attachment; filename="report.txt"
//	/flaky?key=K&fail=N    503 for the first N hits per key, then 200
//	/many-headers          60 response headers
package main

import (
	"compress/flate"
	"compress/gzip"
	"compress/zlib"
	"crypto/ecdsa"
	"crypto/elliptic"
	"crypto/rand"
	"crypto/tls"
	"crypto/x509"
	"crypto/x509/pkix"
	"encoding/base64"
	"fmt"
	"io"
	"log"
	"math/big"
	"mime"
	"mime/multipart"
	"net"
	"net/http"
	"os"
	"sort"
	"strconv"
	"strings"
	"sync"
	"time"
)

var chunk = make([]byte, 64<<10)

const text = "The quick brown fox jumps over the lazy dog.\n"

var (
	flakyMu sync.Mutex
	flaky   = map[string]int{}
)

func echo(w http.ResponseWriter, r *http.Request) {
	var b strings.Builder
	fmt.Fprintf(&b, "%s %s %s\n", r.Method, r.RequestURI, r.Proto)
	if r.Host != "" {
		r.Header.Set("Host", r.Host)
	}
	if len(r.TransferEncoding) > 0 {
		r.Header.Set("Transfer-Encoding", strings.Join(r.TransferEncoding, ","))
	}
	mt, params, _ := mime.ParseMediaType(r.Header.Get("Content-Type"))
	isMultipart := strings.HasPrefix(mt, "multipart/")
	keys := make([]string, 0, len(r.Header))
	for k := range r.Header {
		if k == "User-Agent" || (isMultipart && k == "Content-Length") {
			continue
		}
		keys = append(keys, k)
	}
	sort.Strings(keys)
	for _, k := range keys {
		for _, v := range r.Header[k] {
			if isMultipart && k == "Content-Type" {
				v = mt
			}
			fmt.Fprintf(&b, "%s: %s\n", k, v)
		}
	}
	b.WriteString("\n")
	if isMultipart {
		mr := multipart.NewReader(r.Body, params["boundary"])
		for {
			p, err := mr.NextPart()
			if err != nil {
				break
			}
			data, _ := io.ReadAll(p)
			fmt.Fprintf(&b, "[part name=%q filename=%q type=%q]\n%s\n", p.FormName(), p.FileName(), p.Header.Get("Content-Type"), data)
		}
	} else {
		body, _ := io.ReadAll(r.Body)
		b.Write(body)
	}
	w.Header().Set("Content-Type", "text/plain")
	io.WriteString(w, b.String())
}

func handler() http.Handler {
	mux := http.NewServeMux()
	mux.HandleFunc("/small", func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "text/plain")
		w.Write([]byte("Hello, world\n"))
	})
	mux.HandleFunc("/bytes", func(w http.ResponseWriter, r *http.Request) {
		n, err := strconv.ParseInt(r.URL.Query().Get("n"), 10, 64)
		if err != nil || n < 0 {
			http.Error(w, "bad n", http.StatusBadRequest)
			return
		}
		w.Header().Set("Content-Type", "application/octet-stream")
		w.Header().Set("Content-Length", strconv.FormatInt(n, 10))
		for n > 0 {
			k := int64(len(chunk))
			if n < k {
				k = n
			}
			if _, err := w.Write(chunk[:k]); err != nil {
				return
			}
			n -= k
		}
	})
	mux.HandleFunc("/binary", func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "application/octet-stream")
		w.Write([]byte{0x89, 'P', 'N', 'G', 0, 0, 0, 1, 2, 3})
	})
	mux.HandleFunc("/echo", echo)
	mux.HandleFunc("/echo/", echo)
	mux.HandleFunc("/chunked", func(w http.ResponseWriter, r *http.Request) {
		n, _ := strconv.Atoi(r.URL.Query().Get("n"))
		size, _ := strconv.Atoi(r.URL.Query().Get("size"))
		if size <= 0 {
			size = 10
		}
		f := w.(http.Flusher)
		for i := 0; i < n; i++ {
			w.Write([]byte(strings.Repeat(string(rune('a'+i%26)), size)))
			f.Flush()
		}
	})
	mux.HandleFunc("/status/", func(w http.ResponseWriter, r *http.Request) {
		code, err := strconv.Atoi(strings.TrimPrefix(r.URL.Path, "/status/"))
		if err != nil {
			code = 400
		}
		w.WriteHeader(code)
		if code != 204 && code != 304 {
			fmt.Fprintf(w, "status %d\n", code)
		}
	})
	redirect := func(abs bool) http.HandlerFunc {
		return func(w http.ResponseWriter, r *http.Request) {
			parts := strings.Split(r.URL.Path, "/")
			n, _ := strconv.Atoi(parts[len(parts)-1])
			if n <= 0 {
				fmt.Fprintln(w, "redirect done")
				return
			}
			loc := fmt.Sprintf("/%s/%d", parts[1], n-1)
			if abs {
				scheme := "http"
				if r.TLS != nil {
					scheme = "https"
				}
				loc = scheme + "://" + r.Host + loc
			}
			w.Header().Set("Location", loc)
			w.WriteHeader(302)
			fmt.Fprintln(w, "redirecting")
		}
	}
	mux.HandleFunc("/redirect/", redirect(false))
	mux.HandleFunc("/abs-redirect/", redirect(true))
	mux.HandleFunc("/rel-redirect", func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Location", "echo/rel?x=1")
		w.WriteHeader(302)
	})
	mux.HandleFunc("/r/", func(w http.ResponseWriter, r *http.Request) {
		code, _ := strconv.Atoi(strings.TrimPrefix(r.URL.Path, "/r/"))
		to := r.URL.Query().Get("to")
		if to == "" {
			to = "/echo"
		}
		io.Copy(io.Discard, r.Body)
		w.Header().Set("Location", to)
		w.WriteHeader(code)
	})
	mux.HandleFunc("/basic-auth/", func(w http.ResponseWriter, r *http.Request) {
		parts := strings.Split(r.URL.Path, "/")
		u, p, ok := r.BasicAuth()
		if !ok || len(parts) < 4 || u != parts[2] || p != parts[3] {
			w.Header().Set("WWW-Authenticate", `Basic realm="test"`)
			w.WriteHeader(401)
			fmt.Fprintln(w, "unauthorized")
			return
		}
		fmt.Fprintf(w, "authenticated as %s\n", u)
	})
	mux.HandleFunc("/bearer", func(w http.ResponseWriter, r *http.Request) {
		fmt.Fprintf(w, "auth=%q\n", r.Header.Get("Authorization"))
	})
	mux.HandleFunc("/gzip", func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Encoding", "gzip")
		z := gzip.NewWriter(w)
		for i := 0; i < 200; i++ {
			io.WriteString(z, text)
		}
		z.Close()
	})
	mux.HandleFunc("/deflate", func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Encoding", "deflate")
		z := zlib.NewWriter(w)
		for i := 0; i < 200; i++ {
			io.WriteString(z, text)
		}
		z.Close()
	})
	mux.HandleFunc("/raw-deflate", func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Encoding", "deflate")
		z, _ := flate.NewWriter(w, 6)
		io.WriteString(z, text)
		z.Close()
	})
	mux.HandleFunc("/cookies/set", func(w http.ResponseWriter, r *http.Request) {
		for k, v := range r.URL.Query() {
			http.SetCookie(w, &http.Cookie{Name: k, Value: v[0], Path: "/"})
		}
		w.Header().Set("Location", "/cookies")
		w.WriteHeader(302)
	})
	mux.HandleFunc("/cookies", func(w http.ResponseWriter, r *http.Request) {
		cs := r.Cookies()
		names := make([]string, 0, len(cs))
		for _, c := range cs {
			names = append(names, c.Name+"="+c.Value)
		}
		sort.Strings(names)
		fmt.Fprintf(w, "cookies: %s\n", strings.Join(names, "; "))
	})
	mux.HandleFunc("/slow", func(w http.ResponseWriter, r *http.Request) {
		ms, _ := strconv.Atoi(r.URL.Query().Get("ms"))
		time.Sleep(time.Duration(ms) * time.Millisecond)
		fmt.Fprintln(w, "slow done")
	})
	mux.HandleFunc("/stall", func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Length", "100")
		w.Write([]byte("partial"))
		w.(http.Flusher).Flush()
		select {
		case <-r.Context().Done():
		case <-time.After(30 * time.Second):
		}
	})
	mux.HandleFunc("/http10", func(w http.ResponseWriter, r *http.Request) {
		conn, buf, err := w.(http.Hijacker).Hijack()
		if err != nil {
			return
		}
		defer conn.Close()
		buf.WriteString("HTTP/1.0 200 OK\r\nContent-Type: text/plain\r\n\r\nclose-delimited body\n")
		buf.Flush()
	})
	mux.HandleFunc("/close", func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Connection", "close")
		fmt.Fprintln(w, "closing")
	})
	mux.HandleFunc("/empty", func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Length", "0")
	})
	mux.HandleFunc("/disposition", func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Disposition", `attachment; filename="report.txt"`)
		fmt.Fprintln(w, "report body")
	})
	mux.HandleFunc("/flaky", func(w http.ResponseWriter, r *http.Request) {
		key := r.URL.Query().Get("key")
		fail, _ := strconv.Atoi(r.URL.Query().Get("fail"))
		flakyMu.Lock()
		flaky[key]++
		hit := flaky[key]
		flakyMu.Unlock()
		if hit <= fail {
			w.WriteHeader(503)
			fmt.Fprintf(w, "fail %d\n", hit)
			return
		}
		fmt.Fprintf(w, "ok after %d\n", hit)
	})
	mux.HandleFunc("/many-headers", func(w http.ResponseWriter, r *http.Request) {
		for i := 0; i < 60; i++ {
			w.Header().Set(fmt.Sprintf("X-H%02d", i), strings.Repeat("v", 50))
		}
		fmt.Fprintln(w, "ok")
	})
	return mux
}

// proxy is a minimal forward proxy: absolute-form requests and CONNECT.
// It tags HTTP responses with "X-Via-Test-Proxy: 1" and checks
// Proxy-Authorization when PROXY_AUTH (user:pass) is set.
func proxy() http.Handler {
	want := os.Getenv("PROXY_AUTH")
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if want != "" {
			got := strings.TrimPrefix(r.Header.Get("Proxy-Authorization"), "Basic ")
			if got != base64.StdEncoding.EncodeToString([]byte(want)) {
				w.WriteHeader(407)
				return
			}
		}
		if r.Method == http.MethodConnect {
			dst, err := net.DialTimeout("tcp", r.Host, 5*time.Second)
			if err != nil {
				w.WriteHeader(502)
				return
			}
			conn, buf, _ := w.(http.Hijacker).Hijack()
			conn.Write([]byte("HTTP/1.1 200 Connection established\r\n\r\n"))
			go func() { io.Copy(dst, buf); dst.Close() }()
			io.Copy(conn, dst)
			conn.Close()
			return
		}
		if !r.URL.IsAbs() {
			http.Error(w, "proxy needs absolute-form", 400)
			return
		}
		r.RequestURI = ""
		r.Header.Del("Proxy-Authorization")
		r.Header.Del("Proxy-Connection")
		resp, err := http.DefaultTransport.RoundTrip(r)
		if err != nil {
			http.Error(w, err.Error(), 502)
			return
		}
		defer resp.Body.Close()
		for k, v := range resp.Header {
			w.Header()[k] = v
		}
		w.Header().Set("X-Via-Test-Proxy", "1")
		w.WriteHeader(resp.StatusCode)
		io.Copy(w, resp.Body)
	})
}

func selfSigned() tls.Certificate {
	key, err := ecdsa.GenerateKey(elliptic.P256(), rand.Reader)
	if err != nil {
		log.Fatal(err)
	}
	tmpl := &x509.Certificate{
		SerialNumber: big.NewInt(1),
		Subject:      pkix.Name{CommonName: "localhost"},
		DNSNames:     []string{"localhost"},
		IPAddresses:  []net.IP{net.ParseIP("127.0.0.1")},
		NotBefore:    time.Now().Add(-time.Hour),
		NotAfter:     time.Now().Add(24 * time.Hour),
		KeyUsage:     x509.KeyUsageDigitalSignature,
		ExtKeyUsage:  []x509.ExtKeyUsage{x509.ExtKeyUsageServerAuth},
	}
	der, err := x509.CreateCertificate(rand.Reader, tmpl, tmpl, &key.PublicKey, key)
	if err != nil {
		log.Fatal(err)
	}
	// Write the cert so tests can use --cacert.
	if path := os.Getenv("CERT_OUT"); path != "" {
		f, err := os.Create(path)
		if err == nil {
			fmt.Fprintf(f, "-----BEGIN CERTIFICATE-----\n")
			enc := base64.StdEncoding.EncodeToString(der)
			for len(enc) > 64 {
				fmt.Fprintln(f, enc[:64])
				enc = enc[64:]
			}
			fmt.Fprintln(f, enc)
			fmt.Fprintf(f, "-----END CERTIFICATE-----\n")
			f.Close()
		}
	}
	return tls.Certificate{Certificate: [][]byte{der}, PrivateKey: key}
}

func listen(env, defaultPort string) net.Listener {
	port := os.Getenv(env)
	if port == "" {
		port = defaultPort
	}
	listener, err := net.Listen("tcp4", net.JoinHostPort("127.0.0.1", port))
	if err != nil {
		log.Fatalf("%s=%s: %v", env, port, err)
	}
	return listener
}

func main() {
	httpListener := listen("CRUST_HTTP_PORT", "8080")
	httpsListener := listen("CRUST_HTTPS_PORT", "8443")
	proxyListener := listen("CRUST_PROXY_PORT", "8081")
	h := handler()
	srv := &http.Server{
		Handler:   h,
		TLSConfig: &tls.Config{Certificates: []tls.Certificate{selfSigned()}},
	}
	fmt.Printf("%d %d %d\n", httpListener.Addr().(*net.TCPAddr).Port,
		httpsListener.Addr().(*net.TCPAddr).Port, proxyListener.Addr().(*net.TCPAddr).Port)
	go func() { log.Fatal(http.Serve(proxyListener, proxy())) }()
	go func() { log.Fatal(http.Serve(httpListener, h)) }()
	log.Fatal(srv.ServeTLS(httpsListener, "", ""))
}
