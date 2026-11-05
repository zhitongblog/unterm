import 'package:flutter_test/flutter_test.dart';
import 'package:unterm_mobile/main.dart';

void main() {
  test('isAllowedLanOrTailscaleHost validates LAN and Tailscale ranges', () {
    expect(isAllowedLanOrTailscaleHost('192.168.1.50'), isTrue);
    expect(isAllowedLanOrTailscaleHost('10.0.0.8'), isTrue);
    expect(isAllowedLanOrTailscaleHost('172.20.1.2'), isTrue);
    expect(isAllowedLanOrTailscaleHost('100.88.12.34'), isTrue);
    expect(isAllowedLanOrTailscaleHost('my-pc.tailnet.ts.net'), isTrue);

    expect(isAllowedLanOrTailscaleHost('8.8.8.8'), isFalse);
    expect(isAllowedLanOrTailscaleHost('100.63.0.1'), isFalse);
    expect(isAllowedLanOrTailscaleHost('172.32.0.1'), isFalse);

    expect(isTailscaleHost('100.88.12.34'), isTrue);
    expect(isTailscaleHost('my-pc.tailnet.ts.net'), isTrue);
    expect(isTailscaleHost('192.168.1.50'), isFalse);
  });

  testWidgets('UntermPocketApp renders LAN/Tailscale notice and discovery UI',
      (WidgetTester tester) async {
    await tester.pumpWidget(const UntermPocketApp());
    expect(find.text('Unterm Pocket'), findsOneWidget);
    expect(find.textContaining('仅支持同一局域网'), findsOneWidget);
    expect(find.text('已绑定的电脑'), findsOneWidget);
  });
}
