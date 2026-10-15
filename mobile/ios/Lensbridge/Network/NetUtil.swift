import Foundation
import Network

enum NetUtil {
    /// IPv4 addresses of Wi-Fi / hotspot / USB interfaces, for manual entry on the PC.
    static func localIPv4Addresses() -> [(iface: String, ip: String)] {
        var result: [(String, String)] = []
        var ifaddr: UnsafeMutablePointer<ifaddrs>?
        guard getifaddrs(&ifaddr) == 0, let first = ifaddr else { return [] }
        defer { freeifaddrs(ifaddr) }
        var ptr: UnsafeMutablePointer<ifaddrs>? = first
        while let p = ptr {
            let flags = Int32(p.pointee.ifa_flags)
            if let addr = p.pointee.ifa_addr, addr.pointee.sa_family == UInt8(AF_INET),
               (flags & IFF_UP) != 0, (flags & IFF_LOOPBACK) == 0 {
                let name = String(cString: p.pointee.ifa_name)
                if name.hasPrefix("en") || name.hasPrefix("bridge") || name.hasPrefix("ap") {
                    var host = [CChar](repeating: 0, count: Int(NI_MAXHOST))
                    if getnameinfo(addr, socklen_t(addr.pointee.sa_len), &host, socklen_t(host.count), nil, 0, NI_NUMERICHOST) == 0 {
                        result.append((name, String(cString: host)))
                    }
                }
            }
            ptr = p.pointee.ifa_next
        }
        return result
    }

    /// Only LAN / link-local / loopback peers may connect (no public addresses).
    static func isLocalPeer(_ endpoint: NWEndpoint) -> Bool {
        guard case let .hostPort(host, _) = endpoint else { return false }
        switch host {
        case let .ipv4(a):
            let b = [UInt8](a.rawValue)
            guard b.count == 4 else { return false }
            return b[0] == 10 || b[0] == 127
                || (b[0] == 172 && (16...31).contains(b[1]))
                || (b[0] == 192 && b[1] == 168)
                || (b[0] == 169 && b[1] == 254)
                || (b[0] == 100 && (64...127).contains(b[1])) // CGNAT, used by some hotspots
        case let .ipv6(a):
            let b = [UInt8](a.rawValue)
            guard b.count == 16 else { return false }
            if b.prefix(15).allSatisfy({ $0 == 0 }) && b[15] == 1 { return true } // ::1
            if b[0] == 0xfe && (b[1] & 0xc0) == 0x80 { return true } // fe80::/10
            if (b[0] & 0xfe) == 0xfc { return true } // fc00::/7
            // IPv4-mapped (::ffff:a.b.c.d)
            if b.prefix(10).allSatisfy({ $0 == 0 }) && b[10] == 0xff && b[11] == 0xff {
                if let v4 = IPv4Address(Data(b[12..<16])) {
                    return isLocalPeer(.hostPort(host: .ipv4(v4), port: 1))
                }
            }
            return false
        case .name:
            return false
        @unknown default:
            return false
        }
    }
}
