# No reflection-based serialization (vault JSON uses org.json directly), so the
# default Android optimize rules are sufficient. ZXing's core is plain Java.
-dontwarn com.google.errorprone.annotations.**
