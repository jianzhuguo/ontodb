package io.ontodb;

/**
 * OntoDB Java SDK — client for the OntoDB semantic multi-modal database.
 *
 * <pre>{@code
 * OntoDBClient client = new OntoDBClient("http://localhost:7912", "your-key");
 *
 * // SQL query
 * List<Map<String, Object>> rows = client.query("SELECT * FROM users LIMIT 10");
 * for (Map<String, Object> row : rows) {
 *     System.out.println(row.get("name") + " " + row.get("age"));
 * }
 * }</pre>
 *
 * @see OntoDBClient
 */
public class OntoDB {
    /** SDK version */
    public static final String VERSION = "0.6.1";

    private OntoDB() {} // Utility class
}
