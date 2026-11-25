// Command hysteria-certgen 给 hysteria 插件生成自签 TLS 证书和随机密码。
//
// 为什么包里要自带：插件跑在用户的机器上，那里未必有 openssl，连 od 可能都
// 没有（两者都没有时 hysteria2 就只能起不来，因为入站必须要一对 cert/key）。
// 这里只用 Go 标准库，交叉编译不需要 cgo，所以每个平台都能随包带一个。
//
//	hysteria-certgen cert <cert.pem> <key.pem>   # RSA 2048，自签，10 年
//	hysteria-certgen hex [字节数]                # 默认 16 字节，输出小写十六进制
package main

import (
	"crypto/rand"
	"crypto/rsa"
	"crypto/x509"
	"crypto/x509/pkix"
	"encoding/hex"
	"encoding/pem"
	"fmt"
	"io"
	"math/big"
	"os"
	"strconv"
	"time"
)

// commonName 和以前 openssl 生成的证书保持一致，客户端跳校验或按指纹 pin 都不受影响。
const commonName = "svcd-hysteria"

func main() {
	if len(os.Args) < 2 {
		usage()
	}
	var err error
	switch os.Args[1] {
	case "cert":
		if len(os.Args) != 4 {
			usage()
		}
		err = writeCert(os.Args[2], os.Args[3])
	case "hex":
		count := 16
		if len(os.Args) > 2 {
			if count, err = parseCount(os.Args[2]); err != nil {
				fmt.Fprintf(os.Stderr, "hysteria-certgen: %v\n", err)
				os.Exit(2)
			}
		}
		err = writeHex(count, os.Stdout)
	default:
		usage()
	}
	if err != nil {
		fmt.Fprintf(os.Stderr, "hysteria-certgen: %v\n", err)
		os.Exit(1)
	}
}

func usage() {
	fmt.Fprint(os.Stderr, "usage: hysteria-certgen cert <cert.pem> <key.pem>\n"+
		"       hysteria-certgen hex [bytes]\n")
	os.Exit(2)
}

func parseCount(text string) (int, error) {
	count, err := strconv.Atoi(text)
	if err != nil || count < 1 || count > 256 {
		return 0, fmt.Errorf("字节数必须是 1..256：%q", text)
	}
	return count, nil
}

// writeCert 写一对 RSA 2048 自签证书。先写私钥再写证书：写到一半失败时
// hysteria.sh 看到两件没齐，下次启动会整体重来。
func writeCert(certPath, keyPath string) error {
	key, err := rsa.GenerateKey(rand.Reader, 2048)
	if err != nil {
		return fmt.Errorf("生成私钥：%w", err)
	}
	serial, err := rand.Int(rand.Reader, new(big.Int).Lsh(big.NewInt(1), 128))
	if err != nil {
		return fmt.Errorf("生成序列号：%w", err)
	}
	now := time.Now()
	template := x509.Certificate{
		SerialNumber:          serial,
		Subject:               pkix.Name{CommonName: commonName},
		NotBefore:             now.Add(-time.Hour),
		NotAfter:              now.AddDate(10, 0, 0),
		KeyUsage:              x509.KeyUsageDigitalSignature | x509.KeyUsageKeyEncipherment | x509.KeyUsageCertSign,
		ExtKeyUsage:           []x509.ExtKeyUsage{x509.ExtKeyUsageServerAuth},
		BasicConstraintsValid: true,
		IsCA:                  true,
		DNSNames:              []string{commonName},
	}
	der, err := x509.CreateCertificate(rand.Reader, &template, &template, &key.PublicKey, key)
	if err != nil {
		return fmt.Errorf("生成证书：%w", err)
	}
	keyPEM := pem.EncodeToMemory(&pem.Block{
		Type:  "RSA PRIVATE KEY",
		Bytes: x509.MarshalPKCS1PrivateKey(key),
	})
	if err := writeFile(keyPath, 0o600, keyPEM); err != nil {
		return fmt.Errorf("写私钥 %s：%w", keyPath, err)
	}
	certPEM := pem.EncodeToMemory(&pem.Block{Type: "CERTIFICATE", Bytes: der})
	if err := writeFile(certPath, 0o644, certPEM); err != nil {
		return fmt.Errorf("写证书 %s：%w", certPath, err)
	}
	return nil
}

// writeFile 落盘后显式 chmod：umask 只收紧权限，这里要的是确定值。
func writeFile(path string, perm os.FileMode, data []byte) error {
	if err := os.WriteFile(path, data, perm); err != nil {
		return err
	}
	return os.Chmod(path, perm)
}

func writeHex(count int, out io.Writer) error {
	buf := make([]byte, count)
	if _, err := io.ReadFull(rand.Reader, buf); err != nil {
		return fmt.Errorf("取随机数：%w", err)
	}
	_, err := fmt.Fprintln(out, hex.EncodeToString(buf))
	return err
}
