"""受控 TLS/HTTP 端点；记录线上字节，不依赖客户端内部测试接口。"""
from __future__ import annotations

import asyncio
import base64
import hashlib
import json
import ssl
import struct
import subprocess
import tempfile
import zlib
from pathlib import Path
from typing import Any

from hpack import Decoder, Encoder


def _u16s(data: bytes) -> list[int]:
    if len(data) % 2:
        raise ValueError("odd TLS u16 vector")
    return [int.from_bytes(data[i:i + 2], "big") for i in range(0, len(data), 2)]


def _grease(value: int) -> int | str:
    return "GREASE" if value & 0x0F0F == 0x0A0A and value >> 8 == value & 255 else value


def _trust_anchor_ids(body: bytes) -> list[str]:
    if len(body) < 2 or int.from_bytes(body[:2], "big") != len(body) - 2:
        raise ValueError("malformed TLS trust_anchor_ids vector")
    identities = []
    position = 2
    while position < len(body):
        size = body[position]
        position += 1
        if position + size > len(body):
            raise ValueError("malformed TLS trust anchor ID")
        identities.append(body[position:position + size].hex())
        position += size
    return identities


def _offered_psks(body: bytes) -> dict[str, Any]:
    if len(body) < 4:
        raise ValueError("malformed TLS offered_psks")
    identities_end = 2 + int.from_bytes(body[:2], "big")
    if identities_end > len(body) - 2:
        raise ValueError("malformed TLS PSK identities vector")
    identities = []
    position = 2
    while position < identities_end:
        if position + 2 > identities_end:
            raise ValueError("malformed TLS PSK identity length")
        size = int.from_bytes(body[position:position + 2], "big")
        position += 2
        if position + size + 4 > identities_end:
            raise ValueError("malformed TLS PSK identity")
        identity = body[position:position + size]
        position += size
        identities.append({
            "length": size,
            "sha256": hashlib.sha256(identity).hexdigest(),
            "obfuscated_ticket_age": int.from_bytes(body[position:position + 4], "big"),
        })
        position += 4
    binders_end = position + 2 + int.from_bytes(body[position:position + 2], "big")
    position += 2
    if binders_end != len(body):
        raise ValueError("malformed TLS PSK binders vector")
    binder_lengths = []
    while position < binders_end:
        size = body[position]
        position += 1
        if position + size > binders_end:
            raise ValueError("malformed TLS PSK binder")
        binder_lengths.append(size)
        position += size
    return {"identities": identities, "binder_lengths": binder_lengths}


def observe_client_hello(records: bytes) -> dict[str, Any] | None:
    """跨 TLS record 重组 ClientHello；随机值保留长度而非伪造逐字节相等。"""
    handshake = bytearray()
    record_headers = []
    cursor = 0
    while cursor + 5 <= len(records):
        kind, version, size = struct.unpack("!BHH", records[cursor:cursor + 5])
        if cursor + 5 + size > len(records):
            return None
        record_headers.append([kind, version, size])
        if kind == 22:
            handshake.extend(records[cursor + 5:cursor + 5 + size])
        cursor += 5 + size
        if len(handshake) >= 4 and len(handshake) >= 4 + int.from_bytes(handshake[1:4], "big"):
            break
    if len(handshake) < 4 or len(handshake) < 4 + int.from_bytes(handshake[1:4], "big"):
        return None
    if handshake[0] != 1:
        raise ValueError("expected ClientHello")
    data = bytes(handshake[4:4 + int.from_bytes(handshake[1:4], "big")])
    position = 34
    session_length = data[position]
    position += 1 + session_length
    cipher_length = int.from_bytes(data[position:position + 2], "big")
    position += 2
    ciphers = [_grease(value) for value in _u16s(data[position:position + cipher_length])]
    position += cipher_length
    compression_length = data[position]
    compression = list(data[position + 1:position + 1 + compression_length])
    position += 1 + compression_length
    extension_length = int.from_bytes(data[position:position + 2], "big")
    position += 2
    end = position + extension_length
    extensions: list[dict[str, Any]] = []
    while position < end:
        kind, length = struct.unpack("!HH", data[position:position + 4])
        position += 4
        body = data[position:position + length]
        position += length
        entry: dict[str, Any] = {"type": _grease(kind), "length": length}
        if kind in (10, 13, 50):
            entry["values"] = [_grease(value) for value in _u16s(body[2:])]
        elif kind == 43:
            entry["values"] = [_grease(value) for value in _u16s(body[1:])]
        elif kind in (11, 45):
            entry["values"] = list(body[1:])
        elif kind == 27:
            entry["values"] = _u16s(body[1:])
        elif kind in (16, 17513, 17613):
            protocols = []
            index = 2
            while index < len(body):
                size = body[index]
                protocols.append(body[index + 1:index + 1 + size].decode("ascii"))
                index += size + 1
            entry["protocols"] = protocols
        elif kind == 51:
            shares = []
            index = 2
            while index < len(body):
                group, size = struct.unpack("!HH", body[index:index + 4])
                shares.append([_grease(group), size])
                index += 4 + size
            entry["shares"] = shares
        elif kind == 41:
            entry["offered_psks"] = _offered_psks(body)
        elif kind == 51764:
            entry["trust_anchor_ids"] = _trust_anchor_ids(body)
        elif kind not in (0, 21, 65037) and _grease(kind) != "GREASE":
            entry["hex"] = body.hex()
        extensions.append(entry)
    if position != end:
        raise ValueError("malformed TLS extension vector")
    return {"records": record_headers, "legacy_version": int.from_bytes(data[:2], "big"),
            "session_id_length": session_length, "ciphers": ciphers,
            "compression": compression, "extensions": extensions,
            "raw_client_hello_base64": base64.b64encode(handshake).decode("ascii")}


class _TlsPeer:
    def __init__(self, reader: asyncio.StreamReader, writer: asyncio.StreamWriter,
                 context: ssl.SSLContext, observation: dict[str, Any]):
        self.reader, self.writer, self.observation = reader, writer, observation
        self.incoming, self.outgoing = ssl.MemoryBIO(), ssl.MemoryBIO()
        self.tls = context.wrap_bio(self.incoming, self.outgoing, server_side=True)
        self.wire = bytearray()
        self.plain = bytearray()

    async def flush(self) -> None:
        while self.outgoing.pending:
            self.writer.write(self.outgoing.read())
        await self.writer.drain()

    async def receive(self) -> None:
        await self.flush()
        data = await self.reader.read(65536)
        if not data:
            self.incoming.write_eof()
            raise EOFError("TLS peer closed")
        if "client_hello" not in self.observation:
            self.wire.extend(data)
            hello = observe_client_hello(self.wire)
            if hello is not None:
                self.observation["client_hello"] = hello
                self.wire.clear()
        self.incoming.write(data)

    async def handshake(self) -> None:
        while True:
            try:
                self.tls.do_handshake()
                await self.flush()
                self.observation.update(alpn=self.tls.selected_alpn_protocol(),
                                        tls_version=self.tls.version(), cipher=self.tls.cipher(),
                                        session_reused=self.tls.session_reused)
                return
            except ssl.SSLWantReadError:
                await self.receive()

    async def read(self, size: int = 65536) -> bytes:
        while True:
            try:
                data = self.tls.read(size)
                await self.flush()
                if not data:
                    raise EOFError("TLS peer sent close_notify")
                return data
            except ssl.SSLWantReadError:
                await self.receive()

    async def exact(self, size: int) -> bytes:
        while len(self.plain) < size:
            data = await self.read()
            if not data:
                raise EOFError("TLS plaintext ended")
            self.plain.extend(data)
        data = bytes(self.plain[:size])
        del self.plain[:size]
        return data

    async def until(self, marker: bytes) -> bytes:
        while marker not in self.plain:
            self.plain.extend(await self.read())
            if len(self.plain) > 1024 * 1024:
                raise ValueError("fixture request header limit exceeded")
        end = self.plain.index(marker) + len(marker)
        data = bytes(self.plain[:end])
        del self.plain[:end]
        return data

    async def write(self, data: bytes) -> None:
        view = memoryview(data)
        while view:
            try:
                count = self.tls.write(view)
                view = view[count:]
            except ssl.SSLWantReadError:
                await self.receive()
            await self.flush()


def _frame(kind: int, flags: int, stream: int, payload: bytes = b"") -> bytes:
    return len(payload).to_bytes(3, "big") + bytes([kind, flags]) + struct.pack("!I", stream) + payload


class TransportFixture:
    def __init__(self, openssl: str = "openssl"):
        self.openssl = openssl
        self.connections: list[dict[str, Any]] = []
        self.errors: list[str] = []
        self.tasks: set[asyncio.Task[None]] = set()
        self.server: asyncio.Server | None = None
        self.temporary: tempfile.TemporaryDirectory[str] | None = None
        self.port = 0

    async def __aenter__(self) -> TransportFixture:
        self.temporary = tempfile.TemporaryDirectory(prefix="moli-transport-fixture-")
        directory = Path(self.temporary.name)
        certificate, key = directory / "certificate.pem", directory / "key.pem"
        result = await asyncio.to_thread(subprocess.run, [self.openssl, "req", "-x509", "-newkey", "rsa:2048", "-nodes", "-keyout", str(key), "-out", str(certificate), "-days", "1", "-subj", "/CN=localhost", "-addext", "subjectAltName=DNS:localhost,IP:127.0.0.1"], capture_output=True, check=False)
        if result.returncode:
            self.temporary.cleanup()
            raise RuntimeError(f"local certificate generation failed: {result.stderr.decode(errors='replace')}")
        self.context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        self.context.minimum_version = ssl.TLSVersion.TLSv1_2
        self.context.load_cert_chain(certificate, key)
        self.context.set_alpn_protocols(["h2", "http/1.1"])
        self.server = await asyncio.start_server(self._accept, "127.0.0.1", 0)
        self.port = self.server.sockets[0].getsockname()[1]
        return self

    async def __aexit__(self, *args: object) -> None:
        if self.server:
            self.server.close()
        tasks = list(self.tasks)
        for task in tasks:
            task.cancel()
        await asyncio.gather(*tasks, return_exceptions=True)
        if self.server:
            await self.server.wait_closed()
        if self.temporary:
            self.temporary.cleanup()

    @property
    def url(self) -> str:
        return f"https://localhost:{self.port}"

    def _accept(self, reader: asyncio.StreamReader, writer: asyncio.StreamWriter) -> None:
        task = asyncio.create_task(self._serve(reader, writer))
        self.tasks.add(task)
        task.add_done_callback(self.tasks.discard)

    async def _serve(self, reader: asyncio.StreamReader, writer: asyncio.StreamWriter) -> None:
        observation: dict[str, Any] = {"connection": len(self.connections), "requests": [], "frames": [],
                                       "peer_port": writer.get_extra_info("peername")[1]}
        self.connections.append(observation)
        peer = _TlsPeer(reader, writer, self.context, observation)
        try:
            await peer.handshake()
            if peer.tls.selected_alpn_protocol() == "h2":
                await self._h2(peer, observation)
            else:
                await self._h1(peer, observation)
        except (EOFError, ConnectionError, ssl.SSLEOFError):
            pass
        except ssl.SSLError as error:
            observation["tls_error"] = str(error)
        except asyncio.CancelledError:
            raise
        except Exception as error:
            self.errors.append(f"{type(error).__name__}: {error}")
        finally:
            writer.close()
            try:
                await writer.wait_closed()
            except (ConnectionError, ssl.SSLError):
                pass

    @staticmethod
    def _body(path: str) -> tuple[bytes, str]:
        if path.startswith("/page"):
            # Keep an automatic favicon fetch from changing the measured H2
            # dependency tree while the explicit sequential scenario starts.
            return (
                b"<!doctype html><title>Local transport observation</title>"
                b'<link rel="icon" href="data:,"><body>Transport probe</body>',
                "text/html",
            )
        return bytes([0, 255, 1, 128]) + path.encode(), "application/octet-stream"

    async def _h1(self, peer: _TlsPeer, observation: dict[str, Any]) -> None:
        while True:
            raw = await peer.until(b"\r\n\r\n")
            lines = raw.decode("latin1").split("\r\n")
            method, path, version = lines[0].split(" ", 2)
            headers = [line.split(":", 1) for line in lines[1:] if ":" in line]
            headers = [(name, value.lstrip()) for name, value in headers]
            request = {"method": method, "path": path, "version": version, "headers": headers}
            observation["requests"].append(request)
            lower = {name.lower(): value for name, value in headers}
            length = int(lower.get("content-length", "0"))
            if length:
                request["body_base64"] = base64.b64encode(await peer.exact(length)).decode()
            if lower.get("upgrade", "").lower() == "websocket":
                accept = base64.b64encode(hashlib.sha1((lower["sec-websocket-key"] + "258EAFA5-E914-47DA-95CA-C5AB0DC85B11").encode()).digest())
                subprotocol = b"Sec-WebSocket-Protocol: probe\r\n" if "probe" in lower.get("sec-websocket-protocol", "").split(", ") else b""
                compressed = any(value.strip().split(";", 1)[0] == "permessage-deflate"
                                 for value in lower.get("sec-websocket-extensions", "").split(","))
                extensions = b"Sec-WebSocket-Extensions: permessage-deflate\r\n" if compressed else b""
                await peer.write(b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: " + accept + b"\r\n" + subprotocol + extensions + b"\r\n")
                observation["websocket_compression"] = compressed
                await self._websocket(peer, observation, compressed)
                return
            body, content_type = self._body(path)
            await peer.write(f"HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {len(body)}\r\nCache-Control: no-store\r\n\r\n".encode() + (body if method != "HEAD" else b""))

    async def _h2(self, peer: _TlsPeer, observation: dict[str, Any]) -> None:
        if await peer.exact(24) != b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n":
            raise ValueError("invalid HTTP/2 preface")
        decoder, encoder = Decoder(), Encoder()
        await peer.write(_frame(4, 0, 0))
        blocks: dict[int, bytearray] = {}
        ends_stream: dict[int, bool] = {}
        pending_bodies: dict[int, tuple[dict[str, Any], bytearray]] = {}
        while True:
            head = await peer.exact(9)
            size, kind, flags, stream = int.from_bytes(head[:3], "big"), head[3], head[4], int.from_bytes(head[5:], "big") & 0x7FFFFFFF
            payload = await peer.exact(size)
            frame: dict[str, Any] = {"type": kind, "flags": flags, "stream": stream, "length": size}
            observation["frames"].append(frame)
            if kind == 4 and not flags & 1:
                frame["settings"] = [list(struct.unpack("!HI", payload[i:i + 6])) for i in range(0, size, 6)]
                for setting, value in frame["settings"]:
                    if setting == 1:
                        encoder.header_table_size = value
                await peer.write(_frame(4, 1, 0))
            elif kind == 8:
                frame["increment"] = int.from_bytes(payload, "big") & 0x7FFFFFFF
            elif kind == 6 and not flags & 1:
                await peer.write(_frame(6, 1, 0, payload))
            elif kind in (1, 9):
                if kind == 1:
                    if flags & 8:
                        padding = payload[0]
                        payload = payload[1:len(payload) - padding]
                    if flags & 32:
                        dependency = int.from_bytes(payload[:4], "big")
                        frame["priority"] = {"exclusive": bool(dependency >> 31), "dependency": dependency & 0x7FFFFFFF, "weight": payload[4] + 1}
                        payload = payload[5:]
                    blocks[stream] = bytearray()
                    ends_stream[stream] = bool(flags & 1)
                blocks[stream].extend(payload)
                if flags & 4:
                    headers = decoder.decode(bytes(blocks.pop(stream)))
                    values = dict(headers)
                    path, method = values[":path"], values[":method"]
                    request = {"stream": stream, "method": method, "path": path, "headers": headers}
                    observation["requests"].append(request)
                    if ends_stream.pop(stream):
                        await self._h2_response(peer, encoder, request)
                    else:
                        pending_bodies[stream] = (request, bytearray())
            elif kind == 0:
                if size:
                    await peer.write(_frame(8, 0, 0, struct.pack("!I", size)) + _frame(8, 0, stream, struct.pack("!I", size)))
                if flags & 8:
                    payload = payload[1:len(payload) - payload[0]]
                request, body = pending_bodies[stream]
                body.extend(payload)
                if flags & 1:
                    request["body_base64"] = base64.b64encode(body).decode()
                    del pending_bodies[stream]
                    await self._h2_response(peer, encoder, request)
            elif kind == 7:
                return

    async def _h2_response(self, peer: _TlsPeer, encoder: Encoder, request: dict[str, Any]) -> None:
        body, content_type = self._body(request["path"])
        response_headers = encoder.encode([(":status", "200"), ("content-type", content_type),
                                           ("content-length", str(len(body))), ("cache-control", "no-store")])
        head = request["method"] == "HEAD"
        await peer.write(_frame(1, 5 if head else 4, request["stream"], response_headers))
        if not head:
            await peer.write(_frame(0, 1, request["stream"], body))

    async def _websocket(self, peer: _TlsPeer, observation: dict[str, Any], compression: bool) -> None:
        inflater = zlib.decompressobj(-15) if compression else None
        deflater = zlib.compressobj(wbits=-15) if compression else None
        pending = bytearray()
        message_opcode, message_compressed = 0, False
        while True:
            first, second = await peer.exact(2)
            length = second & 127
            if length == 126:
                length = int.from_bytes(await peer.exact(2), "big")
            elif length == 127:
                length = int.from_bytes(await peer.exact(8), "big")
            mask = await peer.exact(4) if second & 128 else b""
            data = await peer.exact(length)
            if mask:
                data = bytes(value ^ mask[index % 4] for index, value in enumerate(data))
            opcode = first & 15
            if opcode < 8:
                if opcode != 0:
                    message_opcode, message_compressed = opcode, bool(first & 0x40)
                pending.extend(data)
                if not first & 0x80:
                    continue
                opcode, data = message_opcode, bytes(pending)
                pending.clear()
                if message_compressed:
                    if inflater is None:
                        raise ValueError("compressed frame without negotiated extension")
                    data = inflater.decompress(data + b"\x00\x00\xff\xff")
            observation.setdefault("websocket", []).append({
                "opcode": opcode, "compressed": message_compressed if opcode < 8 else False,
                "data_base64": base64.b64encode(data).decode(),
            })
            compressed_reply = opcode in (1, 2) and deflater is not None
            if compressed_reply:
                data = (deflater.compress(data) + deflater.flush(zlib.Z_SYNC_FLUSH))[:-4]
            frame = bytes([0x80 | (0x40 if compressed_reply else 0) | (10 if opcode == 9 else opcode)])
            frame += bytes([len(data)]) if len(data) < 126 else b"\x7e" + struct.pack("!H", len(data))
            await peer.write(frame + data)
            if opcode == 8:
                return


class ConnectProxyFixture:
    """只允许转发到指定本地端点；记录 tunnel 与目标连接的对应关系。"""

    def __init__(self, target_port: int):
        self.target_port = target_port
        self.tunnels: list[dict[str, Any]] = []
        self.tasks: set[asyncio.Task[None]] = set()
        self.errors: list[str] = []

    async def __aenter__(self) -> ConnectProxyFixture:
        self.server = await asyncio.start_server(self._accept, "127.0.0.1", 0)
        self.port = self.server.sockets[0].getsockname()[1]
        return self

    async def __aexit__(self, *args: object) -> None:
        self.server.close()
        tasks = list(self.tasks)
        for task in tasks:
            task.cancel()
        await asyncio.gather(*tasks, return_exceptions=True)
        await self.server.wait_closed()

    def _accept(self, reader: asyncio.StreamReader, writer: asyncio.StreamWriter) -> None:
        task = asyncio.create_task(self._tunnel(reader, writer))
        self.tasks.add(task)
        task.add_done_callback(self.tasks.discard)

    async def _tunnel(self, reader: asyncio.StreamReader, writer: asyncio.StreamWriter) -> None:
        upstream_writer = None
        pumps = []
        try:
            raw = await reader.readuntil(b"\r\n\r\n")
            lines = raw.decode("latin1").split("\r\n")
            if lines[0] != f"CONNECT localhost:{self.target_port} HTTP/1.1":
                writer.write(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n")
                await writer.drain()
                return
            headers = [line.split(":", 1) for line in lines[1:] if ":" in line]
            upstream_reader, upstream_writer = await asyncio.open_connection("127.0.0.1", self.target_port)
            self.tunnels.append({"request_line": lines[0], "headers": headers,
                                 "source_port": upstream_writer.get_extra_info("sockname")[1]})
            writer.write(b"HTTP/1.1 200 Connection Established\r\n\r\n")
            await writer.drain()

            async def pump(source: asyncio.StreamReader, destination: asyncio.StreamWriter) -> None:
                while data := await source.read(65536):
                    destination.write(data)
                    await destination.drain()

            pumps = [asyncio.create_task(pump(reader, upstream_writer)),
                     asyncio.create_task(pump(upstream_reader, writer))]
            await asyncio.wait(pumps, return_when=asyncio.FIRST_COMPLETED)
            for task in pumps:
                if task.done():
                    task.result()
        except (ConnectionError, asyncio.IncompleteReadError):
            pass
        except asyncio.CancelledError:
            raise
        except Exception as error:
            self.errors.append(f"{type(error).__name__}: {error}")
        finally:
            for task in pumps:
                task.cancel()
            await asyncio.gather(*pumps, return_exceptions=True)
            for stream in (writer, upstream_writer):
                if stream:
                    stream.close()
                    try:
                        await stream.wait_closed()
                    except ConnectionError:
                        pass
