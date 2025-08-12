package main

import (
	"context"
	"fmt"
	"io"
	"log"
	"net"
	"os"
	"os/signal"
	"sync"
	"syscall"
	"time"
)

func main() {
	fmt.Printf("PID is %d\n", os.Getpid())

	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()

	sigChan := make(chan os.Signal, 1)
	signal.Notify(sigChan, syscall.SIGINT, syscall.SIGTERM)

	go func() {
		if err := serve(ctx); err != nil {
			log.Printf("Serve failed: %v", err)
		}
	}()

	<-sigChan
	fmt.Println("Received Ctrl + C, shutting down")
	cancel()

	time.Sleep(100 * time.Millisecond)
}

func serve(ctx context.Context) error {
	listenAddr := os.Getenv("EXAMPLE_LISTEN_ADDR")
	if listenAddr == "" {
		listenAddr = "0.0.0.0:5201"
	}

	listener, err := net.Listen("tcp", listenAddr)
	if err != nil {
		return fmt.Errorf("failed to listen on %s: %w", listenAddr, err)
	}
	defer listener.Close()

	fmt.Printf("Listening on %s\n", listenAddr)

	for {
		select {
		case <-ctx.Done():
			return nil
		default:
		}

		if tcpListener, ok := listener.(*net.TCPListener); ok {
			tcpListener.SetDeadline(time.Now().Add(100 * time.Millisecond))
		}

		conn, err := listener.Accept()
		if err != nil {
			if netErr, ok := err.(net.Error); ok && netErr.Timeout() {
				continue
			}
			if netErr, ok := err.(net.Error); ok && netErr.Temporary() {
				log.Printf("Temporary accept error: %v", err)
				continue
			}
			return fmt.Errorf("failed to accept: %w", err)
		}

		remoteAddr := conn.RemoteAddr()
		fmt.Printf("Process incoming connection from %s\n", remoteAddr)

		go forwarding(conn)
	}
}

func forwarding(stream1 net.Conn) error {
	defer stream1.Close()

	remoteAddr := os.Getenv("EXAMPLE_REMOTE_ADDR")
	if remoteAddr == "" {
		remoteAddr = "127.0.0.1:5202"
	}

	stream2, err := net.Dial("tcp", remoteAddr)
	if err != nil {
		log.Printf("Failed to connect to remote server: %v", err)
		return err
	}
	defer stream2.Close()

	startTime := time.Now()

	result, err := copyBidirectional(stream1, stream2)

	elapsed := time.Since(startTime)

	fmt.Printf("Forwarded traffic: %+v, avg: %.4f B/s\n", result, float64((result.BytesForward+result.BytesReverse))/elapsed.Seconds())

	if err != nil {
		log.Printf("Failed to copy data: %v", err)
		return err
	}

	return nil
}

type TrafficStats struct {
	BytesForward uint64
	BytesReverse uint64
}

func (t TrafficStats) String() string {
	return fmt.Sprintf("TrafficStats { bytes_forward: %d, bytes_reverse: %d }",
		t.BytesForward, t.BytesReverse)
}

func copyBidirectional(conn1, conn2 net.Conn) (*TrafficStats, error) {
	var stats TrafficStats
	var wg sync.WaitGroup
	var err1, err2 error

	wg.Add(2)

	go func() {
		defer wg.Done()
		defer conn2.Close()
		n, err := io.Copy(conn2, conn1)
		stats.BytesForward = uint64(n)
		err1 = err
	}()

	go func() {
		defer wg.Done()
		defer conn1.Close()
		n, err := io.Copy(conn1, conn2)
		stats.BytesReverse = uint64(n)
		err2 = err
	}()

	wg.Wait()

	if err1 != nil && err1 != io.EOF {
		return &stats, err1
	}
	if err2 != nil && err2 != io.EOF {
		return &stats, err2
	}

	return &stats, nil
}
