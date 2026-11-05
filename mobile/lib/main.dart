import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

void main() {
  WidgetsFlutterBinding.ensureInitialized();
  runApp(const UntermPocketApp());
}

/// Checks if an IPv4/IPv6 host string is within private LAN, loopback, or Tailscale ranges.
bool isAllowedLanOrTailscaleHost(String host) {
  final h = host.trim().toLowerCase();
  if (h.isEmpty) return false;
  if (h == 'localhost' || h == '127.0.0.1' || h == '::1' || h.endsWith('.local') || h.endsWith('.ts.net')) {
    return true;
  }
  final parts = h.split('.');
  if (parts.length == 4) {
    final octets = parts.map((e) => int.tryParse(e)).toList();
    if (octets.any((e) => e == null || e < 0 || e > 255)) return false;
    final a = octets[0]!;
    final b = octets[1]!;
    if (a == 10 || a == 127) return true;
    if (a == 192 && b == 168) return true;
    if (a == 169 && b == 254) return true;
    if (a == 172 && b >= 16 && b <= 31) return true;
    // Tailscale CGNAT 100.64.0.0/10
    if (a == 100 && b >= 64 && b <= 127) return true;
    return false;
  }
  if (h.startsWith('fd7a:115c:a1e0:') || h.startsWith('fe80:') || h.startsWith('fc') || h.startsWith('fd')) {
    return true;
  }
  return false;
}

/// Returns true if the address is a Tailscale CGNAT / ULA / MagicDNS address.
bool isTailscaleHost(String host) {
  final h = host.trim().toLowerCase();
  if (h.endsWith('.ts.net') || h.startsWith('fd7a:115c:a1e0:')) return true;
  final parts = h.split('.');
  if (parts.length == 4) {
    final a = int.tryParse(parts[0]) ?? -1;
    final b = int.tryParse(parts[1]) ?? -1;
    return a == 100 && b >= 64 && b <= 127;
  }
  return false;
}

class PairedMachine {
  final String machineId;
  String hostname;
  String fingerprint;
  int port;
  String deviceId;
  String deviceToken;
  List<String> addresses;
  String? activeIp;
  bool online;

  PairedMachine({
    required this.machineId,
    required this.hostname,
    required this.fingerprint,
    required this.port,
    required this.deviceId,
    required this.deviceToken,
    required this.addresses,
    this.activeIp,
    this.online = false,
  });

  Map<String, dynamic> toJson() => {
        'machine_id': machineId,
        'hostname': hostname,
        'fingerprint': fingerprint,
        'port': port,
        'device_id': deviceId,
        'device_token': deviceToken,
        'addresses': addresses,
        'active_ip': activeIp,
      };

  factory PairedMachine.fromJson(Map<String, dynamic> j) => PairedMachine(
        machineId: (j['machine_id'] ?? '').toString(),
        hostname: (j['hostname'] ?? 'Unterm-PC').toString(),
        fingerprint: (j['fingerprint'] ?? '').toString(),
        port: (j['port'] is int) ? j['port'] as int : int.tryParse('${j['port']}') ?? 19880,
        deviceId: (j['device_id'] ?? '').toString(),
        deviceToken: (j['device_token'] ?? '').toString(),
        addresses: ((j['addresses'] as List?) ?? const []).map((e) => e.toString()).toList(),
        activeIp: j['active_ip']?.toString(),
      );
}

class DiscoveredHost {
  final String machineId;
  final String hostname;
  final String ip;
  final int port;
  final String fingerprint;
  final List<String> allIps;

  DiscoveredHost({
    required this.machineId,
    required this.hostname,
    required this.ip,
    required this.port,
    required this.fingerprint,
    required this.allIps,
  });
}

class UntermPocketApp extends StatelessWidget {
  const UntermPocketApp({super.key});

  @override
  Widget build(BuildContext context) {
    return MaterialApp(
      title: 'Unterm Pocket',
      debugShowCheckedModeBanner: false,
      theme: ThemeData.dark(useMaterial3: true).copyWith(
        scaffoldBackgroundColor: const Color(0xFF0B0F17),
        colorScheme: const ColorScheme.dark(
          primary: Color(0xFF3B82F6),
          secondary: Color(0xFF10B981),
          surface: Color(0xFF131B2A),
          error: Color(0xFFEF4444),
        ),
        cardTheme: CardThemeData(
          color: const Color(0xFF131B2A),
          shape: RoundedRectangleBorder(
            borderRadius: BorderRadius.circular(12),
            side: const BorderSide(color: Color(0xFF23304A)),
          ),
        ),
      ),
      home: const PocketHomePage(),
    );
  }
}

class PocketHomePage extends StatefulWidget {
  const PocketHomePage({super.key});

  @override
  State<PocketHomePage> createState() => _PocketHomePageState();
}

class _PocketHomePageState extends State<PocketHomePage> {
  List<PairedMachine> _savedMachines = [];
  Map<String, DiscoveredHost> _discovered = {};
  PairedMachine? _connectedMachine;
  bool _scanning = false;
  int _tabIndex = 0;

  // Cockpit live state
  bool _wsConnected = false;
  List<Map<String, dynamic>> _inbox = [];
  List<Map<String, dynamic>> _sessions = [];
  List<Map<String, dynamic>> _approvals = [];
  List<Map<String, dynamic>> _ports = [];
  List<Map<String, dynamic>> _checkpoints = [];
  String _watchedCwd = '';
  String _deviceScopeLabel = '完全控制';
  String _watchedInstance = '';
  int? _watchedPane;
  String _screenText = '';
  double _fontSize = 12.5;
  String _lastAlertSignature = '';

  WebSocket? _ws;
  Timer? _reconnectTimer;
  Timer? _lanSwitchbackTimer;
  final TextEditingController _cmdController = TextEditingController();
  final ScrollController _termScrollController = ScrollController();

  @override
  void initState() {
    super.initState();
    _loadSavedMachines().then((_) => _scanNetworkAndProbes());
  }

  @override
  void dispose() {
    _reconnectTimer?.cancel();
    _lanSwitchbackTimer?.cancel();
    _ws?.close();
    _cmdController.dispose();
    _termScrollController.dispose();
    super.dispose();
  }

  File _storageFile() {
    final dir = Directory.systemTemp;
    return File('${dir.path}/unterm_pocket_machines_v1.json');
  }

  Future<void> _loadSavedMachines() async {
    if (kIsWeb) return;
    try {
      final f = _storageFile();
      if (await f.exists()) {
        final raw = await f.readAsString();
        final list = (jsonDecode(raw) as List)
            .map((e) => PairedMachine.fromJson(Map<String, dynamic>.from(e as Map)))
            .toList();
        if (mounted) {
          setState(() {
            _savedMachines = list;
          });
        }
      }
    } catch (_) {}
  }

  Future<void> _saveMachinesToDisk() async {
    if (kIsWeb) return;
    try {
      final f = _storageFile();
      final data = jsonEncode(_savedMachines.map((m) => m.toJson()).toList());
      await f.writeAsString(data);
    } catch (_) {}
  }

  /// Automatically discovers computers on LAN via UDP broadcast (19880 + mDNS 5353)
  /// AND probes all known LAN / Tailscale addresses of already-paired machines.
  Future<void> _scanNetworkAndProbes() async {
    if (_scanning) return;
    setState(() {
      _scanning = true;
    });

    final found = <String, DiscoveredHost>{};
    final nonce = DateTime.now().millisecondsSinceEpoch.toString();

    // 1. UDP Broadcast & Multicast Discovery on LAN
    if (!kIsWeb) {
      try {
        final socket = await RawDatagramSocket.bind(InternetAddress.anyIPv4, 0);
        socket.broadcastEnabled = true;
        socket.listen((RawSocketEvent event) {
          if (event == RawSocketEvent.read) {
            final dg = socket.receive();
            if (dg == null) return;
            try {
              final text = utf8.decode(dg.data);
              final j = jsonDecode(text) as Map<String, dynamic>;
              if (j['proto'] == 'unterm-discover-v1') {
                final machineId = (j['machine_id'] ?? '').toString();
                final fp = (j['fingerprint'] ?? '').toString();
                final hostname = (j['hostname'] ?? dg.address.address).toString();
                final port = (j['port'] is int) ? j['port'] as int : 19880;
                final addrList = <String>{dg.address.address};
                if (j['addresses'] is List) {
                  for (final a in (j['addresses'] as List)) {
                    if (a is Map && a['ip'] != null) {
                      addrList.add(a['ip'].toString());
                    }
                  }
                }
                if (machineId.isNotEmpty && fp.isNotEmpty) {
                  found[machineId] = DiscoveredHost(
                    machineId: machineId,
                    hostname: hostname,
                    ip: dg.address.address,
                    port: port,
                    fingerprint: fp,
                    allIps: addrList.toList(),
                  );
                }
              }
            } catch (_) {}
          }
        });

        final probeBytes = utf8.encode('UNTERM_DISCOVER_V1:$nonce');
        for (final target in ['255.255.255.255', '127.0.0.1']) {
          try {
            socket.send(probeBytes, InternetAddress(target), 19880);
          } catch (_) {}
        }
        try {
          socket.send(probeBytes, InternetAddress('224.0.0.251'), 5353);
        } catch (_) {}

        await Future.delayed(const Duration(milliseconds: 900));
        socket.close();
      } catch (_) {}
    }

    // 2. Direct HTTP `/api/remote/hello` probe for saved LAN + Tailscale addresses
    final probeFutures = <Future<void>>[];
    for (final m in _savedMachines) {
      final candidates = <String>{
        if (m.activeIp != null && m.activeIp!.isNotEmpty) m.activeIp!,
        ...m.addresses,
      };
      for (final ip in candidates) {
        if (!isAllowedLanOrTailscaleHost(ip)) continue;
        probeFutures.add(() async {
          final host = await _probeHello(ip, m.port, nonce);
          if (host != null && host.machineId == m.machineId && host.fingerprint == m.fingerprint) {
            found[host.machineId] = host;
          }
        }());
      }
    }
    await Future.wait(probeFutures);

    // 3. Reconcile discovered hosts with paired machines (by machine_id + fingerprint)
    bool changed = false;
    for (final m in _savedMachines) {
      final d = found[m.machineId];
      if (d != null && d.fingerprint == m.fingerprint) {
        m.online = true;
        m.activeIp = d.ip;
        m.hostname = d.hostname;
        for (final ip in d.allIps) {
          if (!m.addresses.contains(ip)) {
            m.addresses.add(ip);
            changed = true;
          }
        }
      } else {
        m.online = false;
      }
    }
    if (changed) {
      await _saveMachinesToDisk();
    }

    if (mounted) {
      setState(() {
        _discovered = found;
        _scanning = false;
      });
    }
  }

  Future<DiscoveredHost?> _probeHello(String ip, int port, String nonce) async {
    final client = HttpClient()..connectionTimeout = const Duration(milliseconds: 1200);
    try {
      final uri = Uri.parse('http://$ip:$port/api/remote/hello?nonce=$nonce');
      final req = await client.getUrl(uri);
      final res = await req.close().timeout(const Duration(milliseconds: 1500));
      if (res.statusCode != 200) return null;
      final body = await res.transform(utf8.decoder).join();
      final j = jsonDecode(body) as Map<String, dynamic>;
      final machineId = (j['machine_id'] ?? '').toString();
      final fp = (j['fingerprint'] ?? '').toString();
      final hostname = (j['hostname'] ?? ip).toString();
      final allIps = <String>{ip};
      if (j['addresses'] is List) {
        for (final a in (j['addresses'] as List)) {
          if (a is Map && a['ip'] != null) {
            allIps.add(a['ip'].toString());
          }
        }
      }
      return DiscoveredHost(
        machineId: machineId,
        hostname: hostname,
        ip: ip,
        port: port,
        fingerprint: fp,
        allIps: allIps.toList(),
      );
    } catch (_) {
      return null;
    } finally {
      client.close(force: true);
    }
  }

  Future<void> _pairWithHost({
    required String hostIp,
    required int port,
    required String code,
    String? expectedFingerprint,
  }) async {
    if (!isAllowedLanOrTailscaleHost(hostIp)) {
      _showSnack('安全限制：仅支持局域网（192.168/10/172.16）或 Tailscale（100.64/10）地址', isError: true);
      return;
    }
    final client = HttpClient()..connectionTimeout = const Duration(seconds: 4);
    try {
      final uri = Uri.parse('http://$hostIp:$port/api/remote/pair');
      final req = await client.postUrl(uri);
      req.headers.contentType = ContentType.json;
      req.write(jsonEncode({
        'code': code.trim(),
        'device_name': 'Unterm Android App',
        'platform': 'android',
      }));
      final res = await req.close();
      final raw = await res.transform(utf8.decoder).join();
      final data = jsonDecode(raw) as Map<String, dynamic>;
      if (res.statusCode != 200 || data['device_token'] == null) {
        _showSnack('配对失败：${data['error'] ?? '配对码无效或已过期'}', isError: true);
        return;
      }
      final serverFp = (data['fingerprint'] ?? '').toString();
      if (expectedFingerprint != null &&
          expectedFingerprint.isNotEmpty &&
          serverFp != expectedFingerprint) {
        _showSnack('安全拦截：电脑指纹不匹配！', isError: true);
        return;
      }

      final addrList = <String>{hostIp};
      if (data['addresses'] is List) {
        for (final a in (data['addresses'] as List)) {
          if (a is Map && a['ip'] != null) {
            addrList.add(a['ip'].toString());
          }
        }
      }

      final machine = PairedMachine(
        machineId: (data['machine_id'] ?? '').toString(),
        hostname: (data['hostname'] ?? hostIp).toString(),
        fingerprint: serverFp,
        port: (data['port'] is int) ? data['port'] as int : port,
        deviceId: (data['device_id'] ?? '').toString(),
        deviceToken: data['device_token'].toString(),
        addresses: addrList.toList(),
        activeIp: hostIp,
        online: true,
      );

      setState(() {
        _savedMachines.removeWhere((m) => m.machineId == machine.machineId);
        _savedMachines.insert(0, machine);
      });
      await _saveMachinesToDisk();
      _showSnack('已成功绑定 ${machine.hostname}');
      await _connectToMachine(machine);
    } catch (e) {
      _showSnack('连接失败：$e', isError: true);
    } finally {
      client.close(force: true);
    }
  }

  Future<void> _pairFromUriOrInput(String input) async {
    final trimmed = input.trim();
    if (trimmed.startsWith('unterm://pair')) {
      final uri = Uri.parse(trimmed);
      final hosts = (uri.queryParameters['h'] ?? '').split(',').where((s) => s.isNotEmpty).toList();
      final port = int.tryParse(uri.queryParameters['p'] ?? '19880') ?? 19880;
      final fp = uri.queryParameters['fp'];
      final code = uri.queryParameters['c'] ?? '';
      if (hosts.isEmpty || code.length != 6) {
        _showSnack('配对链接缺少地址或 6 位配对码', isError: true);
        return;
      }
      for (final h in hosts) {
        if (isAllowedLanOrTailscaleHost(h)) {
          await _pairWithHost(hostIp: h, port: port, code: code, expectedFingerprint: fp);
          if (_connectedMachine != null) return;
        }
      }
      return;
    }
    if (trimmed.startsWith('http://') || trimmed.startsWith('https://')) {
      final uri = Uri.parse(trimmed);
      final code = uri.queryParameters['pair'] ?? uri.queryParameters['c'] ?? '';
      final port = uri.hasPort ? uri.port : 19880;
      await _pairWithHost(hostIp: uri.host, port: port, code: code);
      return;
    }
    _showSnack('请输入有效的 unterm://pair 链接或使用下方地址 + 6 位配对码', isError: true);
  }

  /// Happy-Eyeballs style route selection: probes LAN routes first (low latency),
  /// falls back to Tailscale routes, and updates `machine.activeIp`.
  Future<String?> _selectBestRoute(PairedMachine machine) async {
    final nonce = DateTime.now().millisecondsSinceEpoch.toString();
    final lanRoutes = machine.addresses.where((ip) => !isTailscaleHost(ip)).toList();
    final tsRoutes = machine.addresses.where((ip) => isTailscaleHost(ip)).toList();

    for (final ip in [...lanRoutes, ...tsRoutes]) {
      final h = await _probeHello(ip, machine.port, nonce);
      if (h != null && h.machineId == machine.machineId && h.fingerprint == machine.fingerprint) {
        machine.activeIp = ip;
        machine.online = true;
        for (final extra in h.allIps) {
          if (!machine.addresses.contains(extra)) {
            machine.addresses.add(extra);
          }
        }
        return ip;
      }
    }
    return null;
  }

  /// Periodic probe while connected over Tailscale: if a known LAN route becomes
  /// reachable (e.g. user walks back into home/office Wi-Fi), switch back to LAN.
  void _startLanSwitchbackMonitor() {
    _lanSwitchbackTimer?.cancel();
    _lanSwitchbackTimer = Timer.periodic(const Duration(seconds: 15), (_) async {
      final m = _connectedMachine;
      if (m == null || m.activeIp == null || !isTailscaleHost(m.activeIp!)) return;
      final lanCandidates = m.addresses.where((ip) => !isTailscaleHost(ip)).toList();
      if (lanCandidates.isEmpty) return;
      final nonce = DateTime.now().millisecondsSinceEpoch.toString();
      for (final lanIp in lanCandidates) {
        final h = await _probeHello(lanIp, m.port, nonce);
        if (h != null && h.machineId == m.machineId && h.fingerprint == m.fingerprint) {
          if (mounted && _connectedMachine?.machineId == m.machineId) {
            setState(() {
              m.activeIp = lanIp;
            });
            _showSnack('⚡ 已自动从 Tailscale 切回局域网直连 ($lanIp)');
            _connectWebSocket();
          }
          break;
        }
      }
    });
  }

  Future<void> _connectToMachine(PairedMachine machine) async {
    String? targetIp = await _selectBestRoute(machine) ?? machine.activeIp;

    if (targetIp == null) {
      _showSnack('无法连接到 ${machine.hostname}，请确认处于同一局域网 (Wi‑Fi) 或已开启 Tailscale', isError: true);
      return;
    }

    setState(() {
      _connectedMachine = machine;
      _tabIndex = 0;
    });
    _startLanSwitchbackMonitor();
    await _fetchOverview();
    _connectWebSocket();
  }

  void _disconnectMachine() {
    _reconnectTimer?.cancel();
    _lanSwitchbackTimer?.cancel();
    _ws?.close();
    _ws = null;
    setState(() {
      _connectedMachine = null;
      _wsConnected = false;
    });
    _scanNetworkAndProbes();
  }

  Future<void> _fetchOverview() async {
    final m = _connectedMachine;
    if (m == null || m.activeIp == null) return;
    final client = HttpClient()..connectionTimeout = const Duration(seconds: 3);
    try {
      final uri = Uri.parse(
        'http://${m.activeIp}:${m.port}/api/remote/overview'
        '${_watchedPane != null ? '?instance=$_watchedInstance&pane_id=$_watchedPane' : ''}',
      );
      final req = await client.getUrl(uri);
      req.headers.set('Authorization', 'Bearer ${m.deviceToken}');
      final res = await req.close();
      if (res.statusCode == 401) {
        _showSnack('该手机已被电脑端撤销授权，请重新配对', isError: true);
        _disconnectMachine();
        return;
      }
      if (res.statusCode == 200) {
        final raw = await res.transform(utf8.decoder).join();
        final data = jsonDecode(raw) as Map<String, dynamic>;
        _applySnapshot(data);
      }
    } catch (_) {
    } finally {
      client.close(force: true);
    }
  }

  void _connectWebSocket() async {
    _reconnectTimer?.cancel();
    _ws?.close();
    final m = _connectedMachine;
    if (m == null || m.activeIp == null) return;
    final wsUrl =
        'ws://${m.activeIp}:${m.port}/ws?token=${Uri.encodeQueryComponent(m.deviceToken)}';
    try {
      final socket = await WebSocket.connect(wsUrl).timeout(const Duration(seconds: 4));
      _ws = socket;
      if (mounted) {
        setState(() {
          _wsConnected = true;
        });
      }
      if (_watchedPane != null) {
        _sendWs({'type': 'watch', 'instance': _watchedInstance, 'pane_id': _watchedPane});
      }
      socket.listen(
        (dynamic raw) {
          try {
            final msg = jsonDecode(raw.toString()) as Map<String, dynamic>;
            if (msg['type'] == 'snapshot' && msg['data'] is Map) {
              _applySnapshot(Map<String, dynamic>.from(msg['data'] as Map));
            }
          } catch (_) {}
        },
        onDone: _scheduleReconnect,
        onError: (_) => _scheduleReconnect(),
      );
    } catch (_) {
      _scheduleReconnect();
    }
  }

  void _scheduleReconnect() {
    if (!mounted || _connectedMachine == null) return;
    setState(() {
      _wsConnected = false;
    });
    _reconnectTimer?.cancel();
    _reconnectTimer = Timer(const Duration(seconds: 3), () async {
      final m = _connectedMachine;
      if (mounted && m != null) {
        await _selectBestRoute(m);
        _connectWebSocket();
      }
    });
  }

  void _sendWs(Map<String, dynamic> payload) {
    try {
      _ws?.add(jsonEncode(payload));
    } catch (_) {}
  }

  void _applySnapshot(Map<String, dynamic> data) {
    final inbox = ((data['inbox'] as List?) ?? const [])
        .map((e) => Map<String, dynamic>.from(e as Map))
        .toList();
    final sessions = ((data['sessions'] as List?) ?? const [])
        .map((e) => Map<String, dynamic>.from(e as Map))
        .toList();
    final approvals = ((data['approvals'] as List?) ?? const [])
        .map((e) => Map<String, dynamic>.from(e as Map))
        .toList();
    final ports = ((data['ports'] as List?) ?? const [])
        .map((e) => Map<String, dynamic>.from(e as Map))
        .toList();
    final checkpoints = ((data['checkpoints'] as List?) ?? const [])
        .map((e) => Map<String, dynamic>.from(e as Map))
        .toList();
    final screen = data['screen_text']?.toString() ?? _screenText;

    // Auto-learn newly reported LAN / Tailscale routes from the host
    final m = _connectedMachine;
    if (m != null && data['routes'] is List) {
      bool added = false;
      for (final r in (data['routes'] as List)) {
        if (r is Map && r['ip'] != null) {
          final ip = r['ip'].toString();
          if (isAllowedLanOrTailscaleHost(ip) && !m.addresses.contains(ip)) {
            m.addresses.add(ip);
            added = true;
          }
        }
      }
      if (added) {
        _saveMachinesToDisk();
      }
    }

    final alertSig =
        '${inbox.map((e) => '${e['instance_id']}:${e['pane_id'] ?? e['id']}').join(',')}|${approvals.length}';
    if (_lastAlertSignature.isNotEmpty &&
        alertSig != _lastAlertSignature &&
        (inbox.isNotEmpty || approvals.isNotEmpty)) {
      HapticFeedback.heavyImpact();
      SystemSound.play(SystemSoundType.alert);
      if (mounted) {
        _showSnack('🔔 有 Agent 正在等待你确认或回复');
      }
    }
    _lastAlertSignature = alertSig;

    if (!mounted) return;
    setState(() {
      _inbox = inbox;
      _sessions = sessions;
      _approvals = approvals;
      _ports = ports;
      _checkpoints = checkpoints;
      if (data['watched_cwd'] != null) {
        _watchedCwd = data['watched_cwd'].toString();
      }
      if (data['device_scope_label'] != null) {
        _deviceScopeLabel = data['device_scope_label'].toString();
      }
      _screenText = screen;
      if (_watchedPane == null && _sessions.isNotEmpty) {
        _watchedInstance = (_sessions.first['instance_id'] ?? '').toString();
        _watchedPane = _asInt(_sessions.first['pane_id'] ?? _sessions.first['id']);
      }
    });
  }

  int? _asInt(dynamic v) {
    if (v is int) return v;
    if (v != null) return int.tryParse(v.toString());
    return null;
  }

  Future<Map<String, dynamic>?> _callRpc(
    String instance,
    String method,
    Map<String, dynamic> params,
  ) async {
    final m = _connectedMachine;
    if (m == null || m.activeIp == null) return null;
    final client = HttpClient()..connectionTimeout = const Duration(seconds: 4);
    try {
      final uri = Uri.parse('http://${m.activeIp}:${m.port}/api/remote/rpc');
      final req = await client.postUrl(uri);
      req.headers.set('Authorization', 'Bearer ${m.deviceToken}');
      req.headers.contentType = ContentType.json;
      req.write(jsonEncode({
        'instance': instance,
        'method': method,
        'params': params,
      }));
      final res = await req.close();
      final body = await res.transform(utf8.decoder).join();
      return jsonDecode(body) as Map<String, dynamic>;
    } catch (_) {
      return null;
    } finally {
      client.close(force: true);
    }
  }

  Future<void> _sendInput(String instance, int paneId, String text) async {
    HapticFeedback.lightImpact();
    await _callRpc(instance, 'session.input', {'id': paneId, 'data': text});
    await Future.delayed(const Duration(milliseconds: 220));
    await _fetchOverview();
  }

  Future<void> _sendSignal(String instance, int paneId, String sig) async {
    HapticFeedback.mediumImpact();
    await _callRpc(instance, 'signal.send', {'id': paneId, 'signal': sig});
    await Future.delayed(const Duration(milliseconds: 220));
    await _fetchOverview();
  }

  void _openPane(String instance, int paneId) {
    setState(() {
      _watchedInstance = instance;
      _watchedPane = paneId;
      _tabIndex = 2; // Terminal tab
    });
    _sendWs({'type': 'watch', 'instance': instance, 'pane_id': paneId});
    _fetchOverview();
  }

  void _showSnack(String msg, {bool isError = false}) {
    if (!mounted) return;
    ScaffoldMessenger.of(context).showSnackBar(
      SnackBar(
        content: Text(msg),
        backgroundColor: isError ? const Color(0xFFB91C1C) : const Color(0xFF1E293B),
        behavior: SnackBarBehavior.floating,
      ),
    );
  }

  @override
  Widget build(BuildContext context) {
    if (_connectedMachine == null) {
      return _buildMachineDiscoveryScreen();
    }
    return _buildCockpitScreen();
  }

  // ===========================================================================
  // Screen 1: LAN / Tailscale Auto-Discovery & Pairing
  // ===========================================================================
  Widget _buildMachineDiscoveryScreen() {
    final unpairedDiscovered = _discovered.values
        .where((d) => !_savedMachines.any((m) => m.machineId == d.machineId))
        .toList();

    return Scaffold(
      appBar: AppBar(
        backgroundColor: const Color(0xFF0F1522),
        title: const Row(
          children: [
            Icon(Icons.terminal_rounded, color: Color(0xFF3B82F6), size: 22),
            SizedBox(width: 8),
            Text('Unterm Pocket', style: TextStyle(fontSize: 17, fontWeight: FontWeight.w700)),
          ],
        ),
        actions: [
          IconButton(
            tooltip: '扫描局域网 / Tailscale',
            onPressed: _scanning ? null : _scanNetworkAndProbes,
            icon: _scanning
                ? const SizedBox(
                    width: 18,
                    height: 18,
                    child: CircularProgressIndicator(strokeWidth: 2),
                  )
                : const Icon(Icons.radar_rounded),
          ),
        ],
      ),
      body: RefreshIndicator(
        onRefresh: _scanNetworkAndProbes,
        child: ListView(
          padding: const EdgeInsets.all(14),
          children: [
            // Clear LAN / Tailscale scope notice
            Container(
              padding: const EdgeInsets.all(12),
              decoration: BoxDecoration(
                color: const Color(0xFF1E3A8A).withValues(alpha: 0.22),
                borderRadius: BorderRadius.circular(10),
                border: Border.all(color: const Color(0xFF3B82F6).withValues(alpha: 0.45)),
              ),
              child: const Row(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  Icon(Icons.shield_outlined, color: Color(0xFF60A5FA), size: 20),
                  SizedBox(width: 10),
                  Expanded(
                    child: Text(
                      '仅支持同一局域网（Wi‑Fi / 有线）或 Tailscale 连接。\n'
                      'App 启动时会自动扫描本地网络并匹配已绑定的电脑（即使电脑 IP 变化也能自动识别）。',
                      style: TextStyle(fontSize: 12.5, color: Color(0xFFBFDBFE), height: 1.4),
                    ),
                  ),
                ],
              ),
            ),
            const SizedBox(height: 18),

            // Saved / Paired Machines
            Row(
              mainAxisAlignment: MainAxisAlignment.spaceBetween,
              children: [
                const Text(
                  '已绑定的电脑',
                  style: TextStyle(fontSize: 15, fontWeight: FontWeight.w700),
                ),
                if (_scanning)
                  const Text('正在扫描网络…', style: TextStyle(fontSize: 12, color: Color(0xFF60A5FA))),
              ],
            ),
            const SizedBox(height: 8),
            if (_savedMachines.isEmpty)
              const Card(
                child: Padding(
                  padding: EdgeInsets.all(16),
                  child: Text(
                    '暂无已绑定的电脑。请在下方选择自动发现的电脑输入 6 位配对码，或粘贴配对链接。',
                    style: TextStyle(fontSize: 13, color: Color(0xFF8B9BB4)),
                  ),
                ),
              )
            else
              ..._savedMachines.map((m) => Card(
                    child: ListTile(
                      contentPadding: const EdgeInsets.symmetric(horizontal: 14, vertical: 6),
                      leading: Icon(
                        Icons.computer_rounded,
                        color: m.online ? const Color(0xFF10B981) : const Color(0xFF64748B),
                        size: 28,
                      ),
                      title: Row(
                        children: [
                          Text(m.hostname, style: const TextStyle(fontWeight: FontWeight.w700)),
                          const SizedBox(width: 8),
                          Container(
                            padding: const EdgeInsets.symmetric(horizontal: 7, vertical: 2),
                            decoration: BoxDecoration(
                              color: m.online
                                  ? const Color(0xFF10B981).withValues(alpha: 0.18)
                                  : const Color(0xFF64748B).withValues(alpha: 0.18),
                              borderRadius: BorderRadius.circular(99),
                            ),
                            child: Text(
                              m.online ? '在线 · 点击直连' : '离线',
                              style: TextStyle(
                                fontSize: 11,
                                color: m.online ? const Color(0xFF34D399) : const Color(0xFF94A3B8),
                                fontWeight: FontWeight.w600,
                              ),
                            ),
                          ),
                        ],
                      ),
                      subtitle: Padding(
                        padding: const EdgeInsets.only(top: 4),
                        child: Text(
                          '地址: ${m.activeIp ?? m.addresses.join(", ")}:${m.port}\n'
                          '指纹: ${m.fingerprint.length > 16 ? m.fingerprint.substring(0, 16) : m.fingerprint}…',
                          style: const TextStyle(fontSize: 12, color: Color(0xFF8B9BB4)),
                        ),
                      ),
                      trailing: IconButton(
                        icon: const Icon(Icons.delete_outline, color: Color(0xFF94A3B8), size: 20),
                        onPressed: () async {
                          setState(() {
                            _savedMachines.removeWhere((x) => x.machineId == m.machineId);
                          });
                          await _saveMachinesToDisk();
                        },
                      ),
                      onTap: () => _connectToMachine(m),
                    ),
                  )),

            const SizedBox(height: 18),

            // Auto-discovered unpaired machines on LAN
            const Text(
              '局域网自动发现的电脑',
              style: TextStyle(fontSize: 15, fontWeight: FontWeight.w700),
            ),
            const SizedBox(height: 8),
            if (unpairedDiscovered.isEmpty)
              const Card(
                child: Padding(
                  padding: EdgeInsets.all(14),
                  child: Text(
                    '暂未在当前局域网发现未绑定的新电脑（请确保电脑已开启「允许手机远程连接」）。',
                    style: TextStyle(fontSize: 12.5, color: Color(0xFF8B9BB4)),
                  ),
                ),
              )
            else
              ...unpairedDiscovered.map((d) => Card(
                    child: ListTile(
                      leading: const Icon(Icons.wifi_tethering_rounded, color: Color(0xFF3B82F6)),
                      title: Text(d.hostname, style: const TextStyle(fontWeight: FontWeight.w700)),
                      subtitle: Text(
                        '${d.ip}:${d.port} · 指纹 ${d.fingerprint.substring(0, 12)}…',
                        style: const TextStyle(fontSize: 12, color: Color(0xFF8B9BB4)),
                      ),
                      trailing: FilledButton.tonal(
                        onPressed: () => _showPairDialogForHost(d),
                        child: const Text('输入配对码'),
                      ),
                    ),
                  )),

            const SizedBox(height: 18),
            _buildManualPairCard(),
          ],
        ),
      ),
    );
  }

  Widget _buildManualPairCard() {
    final hostCtrl = TextEditingController();
    final codeCtrl = TextEditingController();
    final uriCtrl = TextEditingController();

    return Card(
      child: Padding(
        padding: const EdgeInsets.all(14),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            const Text(
              '扫码链接 / Tailscale / 手动配对',
              style: TextStyle(fontSize: 14.5, fontWeight: FontWeight.w700),
            ),
            const SizedBox(height: 6),
            const Text(
              '在电脑端运行 `unterm-cli remote pair` 或打开「Web 设置 → 手机远程」获取 6 位一次性配对码：',
              style: TextStyle(fontSize: 12, color: Color(0xFF8B9BB4)),
            ),
            const SizedBox(height: 10),
            Row(
              children: [
                Expanded(
                  flex: 3,
                  child: TextField(
                    controller: hostCtrl,
                    decoration: const InputDecoration(
                      isDense: true,
                      labelText: '局域网或 Tailscale IP',
                      hintText: '192.168.x.x / 100.x.x.x',
                      border: OutlineInputBorder(),
                    ),
                  ),
                ),
                const SizedBox(width: 8),
                Expanded(
                  flex: 2,
                  child: TextField(
                    controller: codeCtrl,
                    keyboardType: TextInputType.number,
                    maxLength: 6,
                    decoration: const InputDecoration(
                      isDense: true,
                      counterText: '',
                      labelText: '6 位配对码',
                      border: OutlineInputBorder(),
                    ),
                  ),
                ),
              ],
            ),
            const SizedBox(height: 8),
            SizedBox(
              width: double.infinity,
              child: FilledButton.icon(
                onPressed: () {
                  final rawHost = hostCtrl.text.trim();
                  final code = codeCtrl.text.trim();
                  if (rawHost.isEmpty || code.length != 6) {
                    _showSnack('请填写局域网/Tailscale IP 和 6 位数字配对码', isError: true);
                    return;
                  }
                  final parts = rawHost.split(':');
                  final ip = parts[0];
                  final port = parts.length > 1 ? (int.tryParse(parts[1]) ?? 19880) : 19880;
                  _pairWithHost(hostIp: ip, port: port, code: code);
                },
                icon: const Icon(Icons.link_rounded, size: 18),
                label: const Text('验证并绑定电脑'),
              ),
            ),
            const Divider(height: 22),
            Row(
              children: [
                Expanded(
                  child: TextField(
                    controller: uriCtrl,
                    decoration: const InputDecoration(
                      isDense: true,
                      labelText: '或粘贴 unterm://pair?... 配对链接',
                      border: OutlineInputBorder(),
                    ),
                  ),
                ),
                const SizedBox(width: 8),
                OutlinedButton(
                  onPressed: () => _pairFromUriOrInput(uriCtrl.text),
                  child: const Text('解析绑定'),
                ),
              ],
            ),
          ],
        ),
      ),
    );
  }

  void _showPairDialogForHost(DiscoveredHost host) {
    final ctrl = TextEditingController();
    showDialog(
      context: context,
      builder: (ctx) => AlertDialog(
        title: Text('绑定 ${host.hostname}'),
        content: Column(
          mainAxisSize: MainAxisSize.min,
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Text('地址: ${host.ip}:${host.port}', style: const TextStyle(fontSize: 12.5)),
            Text('指纹: ${host.fingerprint.substring(0, 16)}…',
                style: const TextStyle(fontSize: 12.5)),
            const SizedBox(height: 12),
            TextField(
              controller: ctrl,
              keyboardType: TextInputType.number,
              maxLength: 6,
              autofocus: true,
              decoration: const InputDecoration(
                labelText: '输入电脑端显示的 6 位配对码',
                border: OutlineInputBorder(),
              ),
            ),
          ],
        ),
        actions: [
          TextButton(onPressed: () => Navigator.pop(ctx), child: const Text('取消')),
          FilledButton(
            onPressed: () {
              final code = ctrl.text.trim();
              Navigator.pop(ctx);
              _pairWithHost(
                hostIp: host.ip,
                port: host.port,
                code: code,
                expectedFingerprint: host.fingerprint,
              );
            },
            child: const Text('确认绑定'),
          ),
        ],
      ),
    );
  }

  // ===========================================================================
  // Screen 2: Connected Pocket Cockpit (Inbox / Overview / Terminal / Approvals)
  // ===========================================================================
  Widget _buildCockpitScreen() {
    final m = _connectedMachine!;
    return Scaffold(
      appBar: AppBar(
        backgroundColor: const Color(0xFF0F1522),
        leading: IconButton(
          tooltip: '切换电脑',
          icon: const Icon(Icons.arrow_back_ios_new_rounded, size: 18),
          onPressed: _disconnectMachine,
        ),
        title: Row(
          children: [
            Container(
              width: 9,
              height: 9,
              decoration: BoxDecoration(
                shape: BoxShape.circle,
                color: _wsConnected ? const Color(0xFF10B981) : const Color(0xFFF59E0B),
              ),
            ),
            const SizedBox(width: 8),
            Expanded(
              child: Text(
                '${m.hostname} (${m.activeIp})',
                style: const TextStyle(fontSize: 14.5, fontWeight: FontWeight.w700),
                overflow: TextOverflow.ellipsis,
              ),
            ),
          ],
        ),
        actions: [
          Container(
            margin: const EdgeInsets.symmetric(vertical: 12),
            padding: const EdgeInsets.symmetric(horizontal: 8, vertical: 2),
            decoration: BoxDecoration(
              color: const Color(0xFF10B981).withValues(alpha: 0.16),
              borderRadius: BorderRadius.circular(99),
            ),
            alignment: Alignment.center,
            child: Text(
              _deviceScopeLabel,
              style: const TextStyle(fontSize: 11, color: Color(0xFF34D399), fontWeight: FontWeight.w600),
            ),
          ),
          IconButton(
            tooltip: '立即刷新',
            icon: const Icon(Icons.refresh_rounded, size: 20),
            onPressed: _fetchOverview,
          ),
        ],
      ),
      body: Column(
        children: [
          if (_ports.isNotEmpty)
            Container(
              height: 38,
              color: const Color(0xFF0D1420),
              padding: const EdgeInsets.symmetric(horizontal: 10),
              child: ListView(
                scrollDirection: Axis.horizontal,
                children: [
                  const Center(
                    child: Padding(
                      padding: EdgeInsets.only(right: 8),
                      child: Text('本地预览:', style: TextStyle(fontSize: 11.5, color: Color(0xFF8B9BB4))),
                    ),
                  ),
                  ..._ports.map((p) {
                    final portNum = p['port'];
                    final fw = (p['label'] ?? p['framework_hint'] ?? 'Web').toString();
                    final previewUrl =
                        'http://${m.activeIp}:${m.port}/preview/$portNum/?token=${Uri.encodeQueryComponent(m.deviceToken)}';
                    return Center(
                      child: Padding(
                        padding: const EdgeInsets.only(right: 6),
                        child: ActionChip(
                          visualDensity: VisualDensity.compact,
                          label: Text(':$portNum $fw', style: const TextStyle(fontSize: 11, fontFamily: 'monospace')),
                          onPressed: () {
                            Clipboard.setData(ClipboardData(text: previewUrl));
                            _showSnack('已复制手机预览网址: $previewUrl');
                          },
                        ),
                      ),
                    );
                  }),
                ],
              ),
            ),
          Expanded(
            child: IndexedStack(
              index: _tabIndex,
              children: [
                _buildInboxTab(),
                _buildOverviewTab(),
                _buildTerminalTab(),
                _buildHarnessTab(),
                _buildApprovalsTab(),
              ],
            ),
          ),
        ],
      ),
      bottomNavigationBar: NavigationBar(
        selectedIndex: _tabIndex,
        onDestinationSelected: (idx) => setState(() => _tabIndex = idx),
        backgroundColor: const Color(0xFF0F1522),
        destinations: [
          NavigationDestination(
            icon: Badge(
              isLabelVisible: _inbox.isNotEmpty,
              label: Text('${_inbox.length}'),
              child: const Icon(Icons.inbox_rounded),
            ),
            label: '收件箱',
          ),
          NavigationDestination(
            icon: Badge(
              isLabelVisible: _sessions.isNotEmpty,
              label: Text('${_sessions.length}'),
              child: const Icon(Icons.dashboard_customize_rounded),
            ),
            label: '分屏总览',
          ),
          const NavigationDestination(
            icon: Icon(Icons.terminal_rounded),
            label: '实时终端',
          ),
          NavigationDestination(
            icon: Badge(
              isLabelVisible: _checkpoints.isNotEmpty,
              label: Text('${_checkpoints.length}'),
              child: const Icon(Icons.history_rounded),
            ),
            label: '检查点/接力',
          ),
          NavigationDestination(
            icon: Badge(
              isLabelVisible: _approvals.isNotEmpty,
              label: Text('${_approvals.length}'),
              child: const Icon(Icons.verified_user_rounded),
            ),
            label: '操作审批',
          ),
        ],
      ),
    );
  }

  Widget _buildInboxTab() {
    if (_inbox.isEmpty) {
      return const Center(
        child: Padding(
          padding: EdgeInsets.all(24),
          child: Column(
            mainAxisSize: MainAxisSize.min,
            children: [
              Icon(Icons.check_circle_outline_rounded, size: 46, color: Color(0xFF10B981)),
              SizedBox(height: 12),
              Text('当前没有等待你处理的 Agent', style: TextStyle(fontSize: 15, fontWeight: FontWeight.w600)),
              SizedBox(height: 4),
              Text('当 Claude Code / Codex 需要确认或提问时会自动排在此处并震动提醒',
                  textAlign: TextAlign.center,
                  style: TextStyle(fontSize: 12.5, color: Color(0xFF8B9BB4))),
            ],
          ),
        ),
      );
    }

    return ListView.builder(
      padding: const EdgeInsets.all(12),
      itemCount: _inbox.length,
      itemBuilder: (ctx, i) {
        final item = _inbox[i];
        final inst = (item['instance_id'] ?? 'default').toString();
        final paneId = _asInt(item['pane_id'] ?? item['id']) ?? 0;
        final agentName = (item['agent'] ?? item['name'] ?? 'Agent').toString();
        final summary = (item['summary'] ?? item['reason'] ?? item['title'] ?? '等待确认或输入').toString();

        return Card(
          margin: const EdgeInsets.only(bottom: 10),
          shape: RoundedRectangleBorder(
            borderRadius: BorderRadius.circular(12),
            side: const BorderSide(color: Color(0xFFF59E0B), width: 1.2),
          ),
          child: Padding(
            padding: const EdgeInsets.all(12),
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Row(
                  mainAxisAlignment: MainAxisAlignment.spaceBetween,
                  children: [
                    Text('$agentName ($inst #$paneId)',
                        style: const TextStyle(fontWeight: FontWeight.w700, fontSize: 14.5)),
                    Container(
                      padding: const EdgeInsets.symmetric(horizontal: 8, vertical: 2),
                      decoration: BoxDecoration(
                        color: const Color(0xFFF59E0B).withValues(alpha: 0.2),
                        borderRadius: BorderRadius.circular(99),
                      ),
                      child: const Text('等待你回复',
                          style: TextStyle(fontSize: 11, color: Color(0xFFFBBF24))),
                    ),
                  ],
                ),
                const SizedBox(height: 6),
                Text(summary, style: const TextStyle(fontSize: 13, color: Color(0xFFE2E8F0))),
                const SizedBox(height: 10),
                Wrap(
                  spacing: 8,
                  runSpacing: 8,
                  children: [
                    FilledButton.tonal(
                      onPressed: () => _sendInput(inst, paneId, 'y\r'),
                      child: const Text('同意 (y)'),
                    ),
                    OutlinedButton(
                      onPressed: () => _sendInput(inst, paneId, 'n\r'),
                      child: const Text('拒绝 (n)'),
                    ),
                    OutlinedButton(
                      onPressed: () => _sendInput(inst, paneId, '\r'),
                      child: const Text('Enter'),
                    ),
                    OutlinedButton.icon(
                      onPressed: () => _openPane(inst, paneId),
                      icon: const Icon(Icons.open_in_new_rounded, size: 15),
                      label: const Text('打开终端'),
                    ),
                  ],
                ),
              ],
            ),
          ),
        );
      },
    );
  }

  Widget _buildOverviewTab() {
    if (_sessions.isEmpty) {
      return const Center(
        child: Text('暂无运行中的终端分屏', style: TextStyle(color: Color(0xFF8B9BB4))),
      );
    }
    return ListView.builder(
      padding: const EdgeInsets.all(12),
      itemCount: _sessions.length,
      itemBuilder: (ctx, i) {
        final s = _sessions[i];
        final inst = (s['instance_id'] ?? 'default').toString();
        final paneId = _asInt(s['pane_id'] ?? s['id']) ?? 0;
        final title = (s['title'] ?? s['agent'] ?? 'Pane #$paneId').toString();
        final cwd = (s['cwd'] ?? '').toString();
        final st = (s['agent_state'] ?? s['state'] ?? 'active').toString();

        return Card(
          margin: const EdgeInsets.only(bottom: 8),
          child: ListTile(
            onTap: () => _openPane(inst, paneId),
            leading: const Icon(Icons.terminal_rounded, color: Color(0xFF60A5FA)),
            title: Text(title, style: const TextStyle(fontWeight: FontWeight.w700, fontSize: 14)),
            subtitle: Text('实例: $inst · 分屏 #$paneId\n$cwd',
                style: const TextStyle(fontSize: 12, color: Color(0xFF8B9BB4))),
            trailing: Chip(
              label: Text(st, style: const TextStyle(fontSize: 11)),
              visualDensity: VisualDensity.compact,
            ),
          ),
        );
      },
    );
  }

  Widget _buildTerminalTab() {
    final paneId = _watchedPane ?? 0;
    final inst = _watchedInstance;

    return Column(
      children: [
        // Top pane selector + font zoom
        Container(
          padding: const EdgeInsets.symmetric(horizontal: 10, vertical: 6),
          color: const Color(0xFF0F1624),
          child: Row(
            children: [
              Expanded(
                child: DropdownButtonHideUnderline(
                  child: DropdownButton<String>(
                    isExpanded: true,
                    isDense: true,
                    value: _sessions.any((s) =>
                            (s['instance_id'] ?? '').toString() == inst &&
                            (_asInt(s['pane_id'] ?? s['id']) ?? 0) == paneId)
                        ? '$inst:$paneId'
                        : null,
                    hint: Text('分屏 #$paneId'),
                    items: _sessions.map((s) {
                      final si = (s['instance_id'] ?? '').toString();
                      final sp = _asInt(s['pane_id'] ?? s['id']) ?? 0;
                      final st = (s['title'] ?? 'Pane #$sp').toString();
                      return DropdownMenuItem<String>(
                        value: '$si:$sp',
                        child: Text('$si #$sp · $st',
                            overflow: TextOverflow.ellipsis,
                            style: const TextStyle(fontSize: 13)),
                      );
                    }).toList(),
                    onChanged: (val) {
                      if (val == null) return;
                      final idx = val.indexOf(':');
                      if (idx > 0) {
                        final si = val.substring(0, idx);
                        final sp = int.tryParse(val.substring(idx + 1)) ?? 0;
                        _openPane(si, sp);
                      }
                    },
                  ),
                ),
              ),
              IconButton(
                visualDensity: VisualDensity.compact,
                onPressed: () => setState(() => _fontSize = (_fontSize - 1).clamp(9.0, 22.0)),
                icon: const Icon(Icons.text_decrease_rounded, size: 18),
              ),
              IconButton(
                visualDensity: VisualDensity.compact,
                onPressed: () => setState(() => _fontSize = (_fontSize + 1).clamp(9.0, 22.0)),
                icon: const Icon(Icons.text_increase_rounded, size: 18),
              ),
            ],
          ),
        ),

        // Terminal screen output
        Expanded(
          child: Container(
            width: double.infinity,
            color: const Color(0xFF06090F),
            padding: const EdgeInsets.all(10),
            child: SingleChildScrollView(
              controller: _termScrollController,
              reverse: true,
              child: SelectableText(
                _screenText.isEmpty ? '(等待终端输出…)' : _screenText,
                style: TextStyle(
                  fontFamily: 'monospace',
                  fontSize: _fontSize,
                  height: 1.36,
                  color: const Color(0xFFD8E2F0),
                ),
              ),
            ),
          ),
        ),

        // Quick Key Bar
        Container(
          height: 44,
          color: const Color(0xFF0F1624),
          child: ListView(
            scrollDirection: Axis.horizontal,
            padding: const EdgeInsets.symmetric(horizontal: 8, vertical: 6),
            children: [
              _quickKey('Enter', () => _sendInput(inst, paneId, '\r')),
              _quickKey('y', () => _sendInput(inst, paneId, 'y\r')),
              _quickKey('n', () => _sendInput(inst, paneId, 'n\r')),
              _quickKey('Ctrl+C', () => _sendSignal(inst, paneId, 'SIGINT')),
              _quickKey('Esc', () => _sendInput(inst, paneId, '\x1b')),
              _quickKey('Tab', () => _sendInput(inst, paneId, '\t')),
              _quickKey('↑', () => _sendInput(inst, paneId, '\x1b[A')),
              _quickKey('↓', () => _sendInput(inst, paneId, '\x1b[B')),
              _quickKey('←', () => _sendInput(inst, paneId, '\x1b[D')),
              _quickKey('→', () => _sendInput(inst, paneId, '\x1b[C')),
            ],
          ),
        ),

        // Command / Voice Dictation Input Bar
        Container(
          padding: const EdgeInsets.fromLTRB(8, 6, 8, 8),
          color: const Color(0xFF0F1624),
          child: Row(
            children: [
              Expanded(
                child: TextField(
                  controller: _cmdController,
                  textInputAction: TextInputAction.send,
                  onSubmitted: (_) => _submitCommandLine(inst, paneId),
                  decoration: const InputDecoration(
                    isDense: true,
                    hintText: '输入指令或用键盘麦克风语音输入…',
                    border: OutlineInputBorder(),
                  ),
                ),
              ),
              const SizedBox(width: 8),
              FilledButton(
                onPressed: () => _submitCommandLine(inst, paneId),
                child: const Text('发送'),
              ),
            ],
          ),
        ),
      ],
    );
  }

  Widget _quickKey(String label, VoidCallback onTap) {
    return Padding(
      padding: const EdgeInsets.only(right: 6),
      child: OutlinedButton(
        style: OutlinedButton.styleFrom(
          padding: const EdgeInsets.symmetric(horizontal: 12),
          minimumSize: const Size(0, 32),
        ),
        onPressed: onTap,
        child: Text(label, style: const TextStyle(fontFamily: 'monospace', fontSize: 12.5)),
      ),
    );
  }

  void _submitCommandLine(String inst, int paneId) {
    final text = _cmdController.text;
    if (text.isEmpty) return;
    _cmdController.clear();
    _sendInput(inst, paneId, '$text\r');
  }

  Widget _buildHarnessTab() {
    final cpLabelCtrl = TextEditingController();
    final handoffReqCtrl = TextEditingController();
    final paneId = _watchedPane ?? 0;

    return ListView(
      padding: const EdgeInsets.all(12),
      children: [
        Card(
          child: Padding(
            padding: const EdgeInsets.all(12),
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                const Text(
                  '零污染 Git 检查点 (refs/unterm/checkpoints)',
                  style: TextStyle(fontWeight: FontWeight.w700, fontSize: 14.5),
                ),
                const SizedBox(height: 4),
                const Text(
                  '不改动分支历史与 .git/index，毫秒级保存含未跟踪文件的快照并支持一键回滚',
                  style: TextStyle(fontSize: 12, color: Color(0xFF8B9BB4)),
                ),
                const SizedBox(height: 10),
                Row(
                  children: [
                    Expanded(
                      child: TextField(
                        controller: cpLabelCtrl,
                        decoration: const InputDecoration(
                          isDense: true,
                          hintText: '检查点备注（如：让 Claude 重构前）',
                          border: OutlineInputBorder(),
                        ),
                      ),
                    ),
                    const SizedBox(width: 8),
                    FilledButton(
                      onPressed: () async {
                        final label = cpLabelCtrl.text.trim().isEmpty
                            ? '手机端检查点'
                            : cpLabelCtrl.text.trim();
                        final r = await _callRpc(_watchedInstance, 'checkpoint.create', {
                          'cwd': _watchedCwd,
                          'pane_id': paneId,
                          'label': label,
                          'agent': 'mobile-app',
                        });
                        if (r != null && r['ok'] == true) {
                          _showSnack('已创建零污染 Git 检查点');
                          await _fetchOverview();
                        } else {
                          _showSnack('创建失败：${r?['error'] ?? ''}', isError: true);
                        }
                      },
                      child: const Text('+ 存档'),
                    ),
                  ],
                ),
                const SizedBox(height: 10),
                if (_checkpoints.isEmpty)
                  const Text(
                    '当前仓库暂无隐藏检查点',
                    style: TextStyle(fontSize: 12.5, color: Color(0xFF8B9BB4)),
                  )
                else
                  ..._checkpoints.map((cp) {
                    final cpId = (cp['id'] ?? '').toString();
                    final label = (cp['label'] ?? '').toString();
                    final stat = (cp['stat_summary'] ?? cp['branch'] ?? '').toString();
                    final sha = (cp['commit_sha'] ?? cp['head_sha'] ?? '').toString();
                    final shortSha = sha.length > 8 ? sha.substring(0, 8) : sha;
                    return ListTile(
                      contentPadding: EdgeInsets.zero,
                      dense: true,
                      title: Text('$cpId · $label',
                          style: const TextStyle(fontFamily: 'monospace', fontSize: 12.5, color: Color(0xFF34D399))),
                      subtitle: Text('$shortSha · $stat',
                          style: const TextStyle(fontSize: 11.5, color: Color(0xFF8B9BB4))),
                      trailing: OutlinedButton(
                        onPressed: () async {
                          final r = await _callRpc(_watchedInstance, 'checkpoint.restore', {
                            'cwd': _watchedCwd,
                            'pane_id': paneId,
                            'id': cpId,
                          });
                          if (r != null && r['ok'] == true) {
                            _showSnack('已回滚至检查点 $cpId（已自动备份）');
                            await _fetchOverview();
                          } else {
                            _showSnack('回滚失败：${r?['error'] ?? ''}', isError: true);
                          }
                        },
                        child: const Text('回滚'),
                      ),
                    );
                  }),
              ],
            ),
          ),
        ),
        const SizedBox(height: 10),
        Card(
          child: Padding(
            padding: const EdgeInsets.all(12),
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Text(
                  '跨 Agent 便携接力 & 窗口 A11y 快照 (分屏 #$paneId)',
                  style: const TextStyle(fontWeight: FontWeight.w700, fontSize: 14.5),
                ),
                const SizedBox(height: 4),
                const Text(
                  '自动打包 Git 状态、Diff 与近期终端输出（完整保留你的指令绝不截断），或抓取电脑窗口截图 + UI 控件树注入当前分屏',
                  style: TextStyle(fontSize: 12, color: Color(0xFF8B9BB4)),
                ),
                const SizedBox(height: 10),
                TextField(
                  controller: handoffReqCtrl,
                  decoration: const InputDecoration(
                    isDense: true,
                    hintText: '补充接力指令（永不截断）',
                    border: OutlineInputBorder(),
                  ),
                ),
                const SizedBox(height: 10),
                Wrap(
                  spacing: 8,
                  runSpacing: 8,
                  children: [
                    FilledButton.tonal(
                      onPressed: () async {
                        final reqText = handoffReqCtrl.text.trim().isEmpty
                            ? '请阅读上下文接力文件并继续完成未完事项'
                            : handoffReqCtrl.text.trim();
                        final r = await _callRpc(_watchedInstance, 'handoff.execute', {
                          'cwd': _watchedCwd,
                          'pane_id': paneId,
                          'target_pane_id': paneId,
                          'target_agent': 'claude-code',
                          'user_request': reqText,
                        });
                        if (r != null && r['ok'] == true) {
                          _showSnack('已生成接力包并发送至分屏 #$paneId');
                          setState(() => _tabIndex = 2);
                        } else {
                          _showSnack('接力失败：${r?['error'] ?? ''}', isError: true);
                        }
                      },
                      child: const Text('打包上下文并接力发送'),
                    ),
                    OutlinedButton(
                      onPressed: () async {
                        final r = await _callRpc(_watchedInstance, 'snapshot.capture', {
                          'include_a11y': true,
                          'paste_to_pane': paneId,
                        });
                        if (r != null && r['ok'] == true) {
                          _showSnack('已抓取窗口截图 + A11y 控件树并粘贴至分屏 #$paneId');
                          setState(() => _tabIndex = 2);
                        } else {
                          _showSnack('快照失败：${r?['error'] ?? ''}', isError: true);
                        }
                      },
                      child: const Text('窗口截图 + A11y 控件树粘贴'),
                    ),
                  ],
                ),
              ],
            ),
          ),
        ),
      ],
    );
  }

  Widget _buildApprovalsTab() {
    if (_approvals.isEmpty) {
      return const Center(
        child: Text('当前没有待审批的操作请求', style: TextStyle(color: Color(0xFF8B9BB4))),
      );
    }
    return ListView.builder(
      padding: const EdgeInsets.all(12),
      itemCount: _approvals.length,
      itemBuilder: (ctx, i) {
        final a = _approvals[i];
        final inst = (a['instance_id'] ?? '').toString();
        final id = (a['id'] ?? a['approval_id'] ?? '').toString();
        final action = (a['action'] ?? a['method'] ?? '高风险操作审批').toString();
        final risk = (a['risk'] ?? 'exec').toString();
        final detail = (a['reason'] ?? a['detail'] ?? a.toString()).toString();

        return Card(
          margin: const EdgeInsets.only(bottom: 10),
          child: Padding(
            padding: const EdgeInsets.all(12),
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Row(
                  mainAxisAlignment: MainAxisAlignment.spaceBetween,
                  children: [
                    Text(action, style: const TextStyle(fontWeight: FontWeight.w700)),
                    Chip(label: Text(risk, style: const TextStyle(fontSize: 11))),
                  ],
                ),
                const SizedBox(height: 6),
                Text(detail, style: const TextStyle(fontSize: 12.5, color: Color(0xFFCBD5E1))),
                const SizedBox(height: 10),
                Row(
                  children: [
                    FilledButton(
                      onPressed: () async {
                        await _callRpc(inst, 'approval.decide', {
                          'id': id,
                          'decision': 'approve',
                          'approved': true,
                        });
                        await _fetchOverview();
                      },
                      child: const Text('批准'),
                    ),
                    const SizedBox(width: 8),
                    OutlinedButton(
                      onPressed: () async {
                        await _callRpc(inst, 'approval.decide', {
                          'id': id,
                          'decision': 'deny',
                          'approved': false,
                        });
                        await _fetchOverview();
                      },
                      child: const Text('拒绝'),
                    ),
                  ],
                ),
              ],
            ),
          ),
        );
      },
    );
  }
}
