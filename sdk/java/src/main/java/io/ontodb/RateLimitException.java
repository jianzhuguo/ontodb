package io.ontodb;

/** Rate limit error. */
public class RateLimitException extends OntoDBException {
    public RateLimitException(String message) { super(message); }
}
