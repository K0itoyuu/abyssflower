package fixture;

import java.util.function.Consumer;
import java.util.function.IntConsumer;

public class WriterEdgeFixture<T extends Number & Comparable<T>> {
    private final T value;

    public WriterEdgeFixture(T value) {
        this.value = value;
    }

    public Consumer<String> consumer() {
        return text -> System.out.println(text + value);
    }

    public Consumer<String> boundMethodReference() {
        return this::consume;
    }

    public Consumer<String> nestedLambda() {
        return text -> runInt(number -> consume(text + number));
    }

    private void consume(String text) {
        System.out.println(text);
    }

    private void runInt(IntConsumer consumer) {
        consumer.accept(1);
    }

    public Runnable anonymous() {
        return new Runnable() {
            @Override
            public void run() {
                System.out.println(value);
            }
        };
    }

    public boolean shortCircuit(
            boolean original, boolean disabled, boolean none, boolean accelerationDisabled) {
        return original || (!disabled && !none && !accelerationDisabled);
    }

    public float chooseAndStore(boolean enabled, float custom, float original) {
        float selected = enabled ? custom : original;
        return selected;
    }

    public void receiverBelowConditional(boolean enabled, float left, float right) {
        setRotation(180.0f, enabled ? left : right);
    }

    private void setRotation(float yaw, float pitch) {
        System.out.println(yaw + pitch);
    }

    public String switchValue(String text) {
        return switch (text) {
            case "" -> "empty";
            case "\n" -> "newline";
            default -> text;
        };
    }

    public boolean switchValueWithShortCircuit(int kind, boolean first, boolean second) {
        return switch (kind) {
            case 1 -> false;
            case 2 -> first && second;
            default -> throw new IllegalArgumentException();
        };
    }

    public String nestedSwitchValue(int platform, int modifier) {
        return switch (platform) {
            case 1 -> switch (modifier) {
                case 1 -> "control";
                default -> "other";
            };
            case 2 -> switch (modifier) {
                case 1 -> "command";
                case 2 -> "option";
                default -> "other";
            };
            default -> "other";
        };
    }

    public long switchBehindConditional(boolean enabled, int kind, long fallback) {
        return enabled
                ? switch (kind) {
                    case 1 -> fallback;
                    case 2 -> 42L;
                    default -> throw new IllegalArgumentException();
                }
                : fallback;
    }

    public int latchGuardLoop(int limit) {
        int i = 1;
        int sum = 0;
        if (i <= limit) {
            while (true) {
                sum += i;
                if (i == limit) {
                    break;
                }
                i++;
            }
        }
        return sum;
    }

    public int realDoWhile(int limit) {
        int i = 0;
        do {
            i++;
        } while (i < limit);
        return i;
    }
}
