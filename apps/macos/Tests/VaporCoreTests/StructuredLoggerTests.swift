import Testing

@testable import VaporCore

@Test
func parsesSupportedLogLevelsFromEnvironmentValues() {
  #expect(VaporLogLevel.from(environmentValue: "debug") == .debug)
  #expect(VaporLogLevel.from(environmentValue: "INFO") == .info)
  #expect(VaporLogLevel.from(environmentValue: "warning") == .warning)
  #expect(VaporLogLevel.from(environmentValue: "error") == .error)
  #expect(VaporLogLevel.from(environmentValue: "trace") == nil)
}
