package fixture;

public final class ControlFlowFixture {
    private final String name;

    public ControlFlowFixture(String name) {
        this.name = name;
    }

    public String describe(int value) {
        if (value < 0) {
            return "negative";
        }
        switch (value) {
            case 0:
                return name;
            case 1:
            case 2:
                return "small";
            default:
                return "large-" + value;
        }
    }

    public long doubleValue(long value) {
        return value * 2L;
    }
}
