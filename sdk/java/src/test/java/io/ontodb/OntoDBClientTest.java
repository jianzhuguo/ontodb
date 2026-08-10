package io.ontodb;

import org.junit.jupiter.api.Test;
import static org.junit.jupiter.api.Assertions.*;

class OntoDBClientTest {

    @Test
    void testNew() {
        OntoDBClient client = new OntoDBClient("http://localhost:7912");
        assertNotNull(client);
        assertEquals("OntoDBClient('http://localhost:7912')", client.toString());
    }

    @Test
    void testTrailingSlash() {
        OntoDBClient client = new OntoDBClient("http://localhost:7912/");
        assertEquals("OntoDBClient('http://localhost:7912')", client.toString());
    }

    @Test
    void testWithApiKey() {
        OntoDBClient client = new OntoDBClient("http://localhost:7912", "test-key");
        assertNotNull(client);
    }

    @Test
    void testIsReadyUnreachable() {
        OntoDBClient client = new OntoDBClient("http://127.0.0.1:1", null, 1, 0);
        assertFalse(client.isReady());
    }

    @Test
    void testClose() {
        OntoDBClient client = new OntoDBClient("http://localhost:7912");
        assertDoesNotThrow(() -> client.close());
    }

    @Test
    void testExceptions() {
        OntoDBException base = new OntoDBException("test");
        assertEquals("test", base.getMessage());

        ConnectionException conn = new ConnectionException("conn");
        assertTrue(conn instanceof OntoDBException);

        QueryException query = new QueryException("query");
        assertTrue(query instanceof OntoDBException);

        AuthenticationException auth = new AuthenticationException("auth");
        assertTrue(auth instanceof OntoDBException);

        RateLimitException rate = new RateLimitException("rate");
        assertTrue(rate instanceof OntoDBException);
    }

    @Test
    void testInsertManyEmpty() {
        OntoDBClient client = new OntoDBClient("http://localhost:7912");
        assertDoesNotThrow(() -> client.insertMany("users", null));
        assertDoesNotThrow(() -> client.insertMany("users", java.util.Collections.emptyList()));
    }
}
