import Testing

@testable import VaporCore

@Test
func releaseVersionStripsPrereleaseSuffix() {
  #expect(VaporBuildInfo.releaseVersion(from: "0.2.0") == "0.2.0")
  #expect(VaporBuildInfo.releaseVersion(from: "0.2.0-rc.1") == "0.2.0")
  #expect(VaporBuildInfo.releaseVersion(from: "1.4.3-beta.2") == "1.4.3")
}

@Test
func displayVersionAlwaysContainsVersion() {
  #expect(VaporBuildInfo.displayVersion.contains(VaporBuildInfo.version))
}
