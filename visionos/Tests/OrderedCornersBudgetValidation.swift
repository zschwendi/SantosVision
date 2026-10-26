import Foundation

@main
struct OrderedCornersBudgetValidation {
    static func main() {
        var checks = 0
        func require(_ value: @autoclosure () -> Bool, _ message: String) {
            precondition(value(), message); checks += 1
        }
        typealias Budget = GameWorldOrderedCornersBudget
        require(Budget.maximumSets == 6 && Budget.maximumBytes == 402_653_184, "Explicit pool bounds")
        for (width, height) in [(1, 1), (640, 360), (960, 540), (1920, 1080), (2560, 1440), (2560, 2304), (3840, 2160)] {
            let item = Budget.layout(width: width, height: height, maxBufferLength: Int.max)!
            require(item.cornerWidth == width + 1 && item.cornerHeight == height + 1, "Full shared corner grid")
            require(item.indexCount == width * height * 6 && item.indexBytes == width * height * 24,
                "One UInt32 per original triangle index, never truncation or compacted order")
            require(item.cornerBytes == (width + 1) * (height + 1) * 4,
                "One R32Float for every geometric corner")
            require(item.bytes == item.cornerBytes + item.indexBytes, "Include both resources in admission")
            require(Budget.layout(width: width, height: height, maxBufferLength: item.indexBytes) == item,
                "Exact device buffer bound is inclusive")
            require(Budget.layout(width: width, height: height, maxBufferLength: item.indexBytes - 1) == nil,
                "Actual device buffer bound cannot be exceeded")
        }
        let tall = Budget.layout(width: 2560, height: 2304, maxBufferLength: Int.max)!
        require(tall.bytes == 165_170_180, "Measured-format nominal allocation is 157.52 MiB per eye")
        require(Budget.canAdd(bytes: tall.bytes, heldBytes: tall.bytes, heldSets: 1), "Two tall sets fit")
        require(!Budget.canAdd(bytes: tall.bytes, heldBytes: tall.bytes * 2, heldSets: 2), "Third tall set exceeds total budget")
        for invalid in [(0, 1), (1, 0), (-1, 100), (100, -1), (Int.max, 1), (1, Int.max),
                        (Int.min, Int.max), (16_384, 1), (1, 16_384), (16_383, 16_383)] {
            require(Budget.layout(width: invalid.0, height: invalid.1, maxBufferLength: Int.max) == nil,
                "Unbounded dimensions, arithmetic overflow and oversized allocations reject before allocation")
        }
        for limit in [0, -1, Int.min] {
            require(Budget.layout(width: 3, height: 3, maxBufferLength: limit) == nil, "Invalid device bound")
        }
        require(Budget.canAdd(bytes: Budget.maximumBytes, heldBytes: 0, heldSets: 0), "Exact total budget inclusive")
        require(!Budget.canAdd(bytes: Budget.maximumBytes, heldBytes: 1, heldSets: 0), "One byte over bound rejects")
        require(!Budget.canAdd(bytes: Int.max, heldBytes: 1, heldSets: 0), "Adding bytes cannot overflow")
        require(!Budget.canAdd(bytes: 1, heldBytes: Int.max, heldSets: 0), "Held byte arithmetic cannot overflow")
        require(Budget.canAdd(bytes: 1, heldBytes: 0, heldSets: 5), "Sixth set permitted")
        require(!Budget.canAdd(bytes: 1, heldBytes: 0, heldSets: 6), "Seventh set rejects")
        require(!Budget.canAdd(bytes: 1, heldBytes: -1, heldSets: 0), "Invalid held state rejects")
        require(!Budget.canAdd(bytes: 1, heldBytes: 0, heldSets: -1), "Invalid set count rejects")
        require(!Budget.canAdd(bytes: 0, heldBytes: 0, heldSets: 0), "Empty resource admission rejects")
        print("PASS ordered corners budget: \(checks) overflow, device buffer, exact bytes, total memory and set-limit checks")
    }
}
