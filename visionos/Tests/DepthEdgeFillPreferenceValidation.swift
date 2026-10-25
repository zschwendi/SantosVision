import Foundation

let suite = "LosSantosDepthFillPreferenceTest.\(UUID().uuidString)"
let defaults = UserDefaults(suiteName: suite)!
defer { defaults.removePersistentDomain(forName: suite) }
let policy = GameWorldDepthEdgeFillPolicy.self
func check(_ value: @autoclosure () -> Bool, _ message: String) {
    precondition(value(), message)
}

check(defaults.object(forKey: policy.storedKey) == nil, "isolated fresh preferences")
check(policy.savedChoice(in: defaults), "restored27 repair survives an ordinary launch without QA flags")
check(policy.requested(environment: [:], arguments: [], stored: policy.savedChoice(in: defaults)), "normal renderer selects repair")

defaults.set(false, forKey: policy.storedKey)
check(!policy.savedChoice(in: defaults), "explicit launcher opt-out is preserved")
check(!policy.savedChoice(in: UserDefaults(suiteName: suite)!), "a new preferences reader retains the opt-out")
check(!policy.requested(environment: [:], arguments: [], stored: policy.savedChoice(in: defaults)), "normal renderer respects saved off")

defaults.set(true, forKey: policy.storedKey)
check(policy.savedChoice(in: UserDefaults(suiteName: suite)!), "a new reader retains the enabled choice")
check(!policy.requested(environment: [:], arguments: ["--sv-world-disable-depth-edge-fill"], stored: policy.savedChoice(in: defaults)), "explicit QA disable still wins")
check(!policy.requested(environment: ["MUGUET_COMPOSITOR_DEPTH_EDGE_FILL": "0"], arguments: [], stored: policy.savedChoice(in: defaults)), "environment disable still wins")
check(policy.requested(environment: ["MUGUET_COMPOSITOR_DEPTH_EDGE_FILL": "1"], arguments: [], stored: false), "controlled QA enable remains available")
print("Depth-edge repair preference: 10 checks passed")
