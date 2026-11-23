// The sandbox controller invokes this static binary from the installation
// image. A custom image receives it through a read-only volume.
package main

import (
	"bufio"
	"context"
	"encoding/binary"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net"
	"net/http"
	"net/url"
	"os"
	"os/exec"
	"strconv"
	"strings"
	"syscall"
	"time"
	"unsafe"
)

const (
	dockerCA = "/workspace/.runbook-ca.pem"
	kubeCA   = "/tmp/.runbook-ca.pem"
)

type request struct {
	Argv           []string          `json:"argv"`
	Stdin          string            `json:"stdin"`
	Cwd            string            `json:"cwd"`
	Timeout        float64           `json:"timeout_s"`
	Env            map[string]string `json:"env"`
	ProxyURL       string            `json:"proxy_url"`
	ListenerSecret string            `json:"listener_secret"`
	CA             string            `json:"ca_pem"`
}

func packet() (request, error) {
	var input request
	var size uint32
	if err := binary.Read(os.Stdin, binary.BigEndian, &size); err != nil || size > 1<<20 {
		return input, errors.New("invalid request size")
	}
	body := make([]byte, size)
	if _, err := io.ReadFull(os.Stdin, body); err != nil {
		return input, err
	}
	err := json.Unmarshal(body, &input)
	return input, err
}

func commandEnv(input request) []string {
	proxy, err := url.Parse(input.ProxyURL)
	if err != nil || proxy.Scheme != "http" || proxy.Host == "" {
		return nil
	}
	proxy.User = url.UserPassword("runbook", input.ListenerSecret)
	env := make(map[string]string)
	for _, item := range os.Environ() {
		if name, value, ok := strings.Cut(item, "="); ok {
			env[name] = value
		}
	}
	for name, value := range input.Env {
		env[name] = value
	}
	for _, name := range []string{"HTTPS_PROXY", "HTTP_PROXY", "https_proxy", "http_proxy"} {
		env[name] = proxy.String()
	}
	for _, name := range []string{"NO_PROXY", "no_proxy", "ALL_PROXY", "all_proxy"} {
		env[name] = ""
	}
	for _, name := range []string{"SSL_CERT_FILE", "CURL_CA_BUNDLE", "REQUESTS_CA_BUNDLE", "NODE_EXTRA_CA_CERTS", "GIT_SSL_CAINFO", "PIP_CERT"} {
		env[name] = kubeCA
	}
	env["HOME"] = "/workspace"
	values := make([]string, 0, len(env))
	for name, value := range env {
		values = append(values, name+"="+value)
	}
	return values
}

func drainChildren() bool {
	deadline := time.Now().Add(2 * time.Second)
	parent := strconv.Itoa(os.Getpid())
	for time.Now().Before(deadline) {
		var status syscall.WaitStatus
		pid, err := syscall.Wait4(-1, &status, syscall.WNOHANG, nil)
		if err == syscall.ECHILD {
			return true
		}
		if pid > 0 || err == syscall.EINTR {
			continue
		}
		if err != nil {
			return false
		}
		// The kernel reparents all living grandchildren to the subreaper
		// as their parents exit. This also catches descendants in other sessions.
		entries, err := os.ReadDir("/proc")
		if err != nil {
			return false
		}
		for _, entry := range entries {
			pid, err := strconv.Atoi(entry.Name())
			if err != nil {
				continue
			}
			stat, err := os.ReadFile("/proc/" + entry.Name() + "/stat")
			end := strings.LastIndexByte(string(stat), ')')
			if err != nil || end < 0 {
				continue
			}
			fields := strings.Fields(string(stat[end+1:]))
			if len(fields) >= 2 && fields[1] == parent {
				_ = syscall.Kill(pid, syscall.SIGKILL)
			}
		}
		time.Sleep(10 * time.Millisecond)
	}
	return false
}

func setup(argv []string, cwd string, timeout time.Duration, stdin *os.File, env []string) int {
	if len(argv) == 0 || timeout <= 0 {
		return 125
	}
	const setDumpable = 4
	if _, _, err := syscall.RawSyscall6(syscall.SYS_PRCTL, setDumpable, 0, 0, 0, 0, 0); err != 0 {
		return 125
	}
	const setChildSubreaper = 36
	if _, _, err := syscall.RawSyscall6(syscall.SYS_PRCTL, setChildSubreaper, 1, 0, 0, 0, 0); err != 0 {
		return 125
	}
	command := exec.Command(argv[0], argv[1:]...)
	command.Dir, command.Env = cwd, env
	command.Stdin, command.Stdout, command.Stderr = stdin, os.Stdout, os.Stderr
	command.SysProcAttr = &syscall.SysProcAttr{Setpgid: true}
	if err := command.Start(); err != nil {
		return 125
	}
	wait := make(chan error, 1)
	go func() { wait <- command.Wait() }()
	timer := time.NewTimer(timeout)
	defer timer.Stop()
	status := 0
	select {
	case err := <-wait:
		if err != nil {
			status = command.ProcessState.ExitCode()
			if status < 0 {
				status = 125
			}
		}
	case <-timer.C:
		_ = syscall.Kill(-command.Process.Pid, syscall.SIGKILL)
		// The controller watchdog deletes the sandbox if the kernel cannot
		// confirm that the setup process exited.
		<-wait
		status = 124
	}
	_ = syscall.Kill(-command.Process.Pid, syscall.SIGKILL)
	if !drainChildren() {
		return 125
	}
	return status
}

func runtimeProbe() int {
	const syslogReadAll = 3
	log := make([]byte, 128<<10)
	size, _, err := syscall.Syscall(syscall.SYS_SYSLOG, syslogReadAll, uintptr(unsafe.Pointer(&log[0])), uintptr(len(log)))
	if err != 0 || size > uintptr(len(log)) || !strings.Contains(string(log[:size]), "Starting gVisor") {
		return 1
	}
	return 0
}

func connect(proxy *url.URL, target string, authenticated bool) int {
	conn, err := net.DialTimeout("tcp", proxy.Host, 2*time.Second)
	if err != nil {
		return 0
	}
	defer conn.Close()
	_ = conn.SetDeadline(time.Now().Add(2 * time.Second))
	req := &http.Request{Method: http.MethodConnect, URL: &url.URL{Host: target}, Host: target, Header: make(http.Header)}
	if authenticated {
		password, _ := proxy.User.Password()
		req.SetBasicAuth("runbook", password)
		req.Header.Set("Proxy-Authorization", req.Header.Get("Authorization"))
		req.Header.Del("Authorization")
	}
	if err := req.Write(conn); err != nil {
		return 0
	}
	res, err := http.ReadResponse(bufio.NewReader(conn), req)
	if err != nil {
		return 0
	}
	return res.StatusCode
}

func isolationProbe(kube bool, rawProxy, bridge string) int {
	proxy, err := url.Parse(rawProxy)
	if err != nil || proxy.Scheme != "http" || proxy.Port() == "" || proxy.User == nil {
		return 1
	}
	ca := dockerCA
	targets := map[string][]string{
		"169.254.169.254": {"80", "443"}, "metadata.google.internal": {"80", "443"},
		"169.254.170.2": {"80", "443"}, "169.254.170.23": {"80", "443"},
		"fd00:ec2::254": {"80", "443"}, "1.1.1.1": {"80", "443"},
	}
	domains := []string{"example.com"}
	if kube {
		ca = kubeCA
		targets["169.254.169.252"] = []string{"987", "988"}
		domains = append(domains, "kubernetes.default.svc.cluster.local")
	} else {
		status, err := os.ReadFile("/proc/self/status")
		if err != nil || !strings.Contains(string(status), "\nSeccomp:\t2\n") {
			return 1
		}
		if bridge != "" {
			targets[bridge] = []string{"80", "443"}
		}
	}
	if info, err := os.Stat(ca); err != nil || !info.Mode().IsRegular() {
		return 1
	}
	for _, tool := range []string{"/bin/bash", "/usr/local/bin/node", "/usr/bin/rg", "/usr/bin/timeout"} {
		if info, err := os.Stat(tool); err != nil || !info.Mode().IsRegular() || info.Mode().Perm()&0111 == 0 {
			return 1
		}
	}
	if kube {
		if info, err := os.Stat("/usr/local/lib/runbook-sandbox.mjs"); err != nil || !info.Mode().IsRegular() {
			return 1
		}
	}

	results := make(chan int)
	count := 0
	check := func(f func() int) {
		count++
		go func() { results <- f() }()
	}
	for host, ports := range targets {
		for _, port := range ports {
			address := net.JoinHostPort(host, port)
			check(func() int {
				if conn, err := net.DialTimeout("tcp", address, time.Second); err == nil {
					conn.Close()
					return 20
				}
				return 0
			})
			check(func() int {
				switch code := connect(proxy, address, true); {
				case code >= 200 && code < 300:
					return 20
				case code != 403:
					return 1
				default:
					return 0
				}
			})
		}
	}
	for _, host := range domains {
		host := host
		check(func() int {
			ctx, cancel := context.WithTimeout(context.Background(), time.Second)
			defer cancel()
			if records, err := net.DefaultResolver.LookupIP(ctx, "ip4", host+"."); err == nil && len(records) != 0 {
				return 20
			}
			return 0
		})
	}
	if kube {
		check(func() int {
			if connect(proxy, "169.254.169.254:80", false) != 407 {
				return 1
			}
			return 0
		})
	}
	code := 0
	for ; count > 0; count-- {
		if result := <-results; result > code {
			code = result
		}
	}
	return code
}

func writeCA(path, value string) error {
	file, err := os.OpenFile(path, os.O_WRONLY|os.O_CREATE|os.O_EXCL, 0400)
	if err != nil {
		return err
	}
	defer file.Close()
	_, err = file.WriteString(value)
	return err
}

func main() {
	if len(os.Args) >= 2 {
		switch os.Args[1] {
		case "version":
			return
		case "idle":
			if len(os.Args) == 2 {
				for {
					time.Sleep(time.Hour)
				}
			}
			ms, err := strconv.Atoi(os.Args[2])
			if err == nil && ms > 0 && writeCA(dockerCA, os.Getenv("RUNBOOK_SANDBOX_CA")) == nil {
				time.Sleep(time.Duration(ms) * time.Millisecond)
				return
			}
		case "attest":
			os.Exit(runtimeProbe())
		case "probe-docker":
			if len(os.Args) == 3 {
				os.Exit(isolationProbe(false, os.Getenv("HTTPS_PROXY"), os.Args[2]))
			}
		case "setup-docker":
			if len(os.Args) >= 4 {
				ms, err := strconv.Atoi(os.Args[2])
				if err == nil {
					cwd, err := os.Getwd()
					if err == nil {
						os.Exit(setup(os.Args[3:], cwd, time.Duration(ms)*time.Millisecond, os.Stdin, nil))
					}
				}
			}
		case "init", "probe-kube", "setup-kube":
			input, err := packet()
			if err != nil {
				break
			}
			switch os.Args[1] {
			case "init":
				if writeCA(kubeCA, input.CA) == nil {
					return
				}
			case "probe-kube":
				proxy, err := url.Parse(input.ProxyURL)
				if err == nil {
					proxy.User = url.UserPassword("runbook", input.ListenerSecret)
					os.Exit(isolationProbe(true, proxy.String(), ""))
				}
			case "setup-kube":
				stdin, err := os.CreateTemp("/tmp", ".setup-stdin-")
				if err == nil {
					_ = os.Remove(stdin.Name())
					_, err := stdin.WriteString(input.Stdin)
					if err == nil {
						_, err = stdin.Seek(0, 0)
					}
					if env := commandEnv(input); err == nil && env != nil {
						os.Exit(setup(input.Argv, input.Cwd, time.Duration(input.Timeout*float64(time.Second)), stdin, env))
					}
				}
			}
		}
	}
	fmt.Fprintln(os.Stderr, "sandbox helper failed")
	os.Exit(125)
}
